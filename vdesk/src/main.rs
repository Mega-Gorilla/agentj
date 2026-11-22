//! OpenDeck plugin: Windows 11 virtual desktops and AI agent sessions.
//!
//! Desktop actions:
//! - Desktop (slot): slot keys on a device are numbered in reading order
//!   (row, then column). Slot N switches to desktop N. Slots beyond the number
//!   of desktops go dark, so the keys follow desktops being added or removed.
//! - Desktop Info: "current / total" and the current name. Press to re-read.
//! - Previous / Next desktop.
//!
//! AI actions (fed by Claude Code / Codex hooks, see agents.rs):
//! - AI Session (slot): one running session per key, coloured by state.
//!   Press to go to that session's desktop. A key whose settings say
//!   `"tool": "claude"` or `"codex"` shows only that tool's sessions; keys
//!   without one list sessions of tools that have no row. Keys for a tool
//!   make columns, one desktop per column, Claude above Codex.
//! - AI Page Previous / Next: when the columns do not fit, page through them.
//!   Orange when a session on a page that way needs the user.
//! - AI Refresh: re-read names and windows now, remove sessions that look
//!   closed, and send every image again.
//! - AI Summary: how many need you / have answered / are working. Press to
//!   go to the session that needs you most; press again to go to the next.
//!
//! `vdesk.exe --selftest` prints what it reads from Windows and writes
//! preview.html, without switching anything.

mod agents;
mod front;
mod keys;
mod prefs;
mod render;
mod vd;
mod winutil;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use openaction::*;
use tokio::sync::Mutex;

const SLOT: ActionUuid = "local.virtualdesktop.slot";
const INFO: ActionUuid = "local.virtualdesktop.info";
const PREV: ActionUuid = "local.virtualdesktop.prev";
const NEXT: ActionUuid = "local.virtualdesktop.next";
const AGENT: ActionUuid = "local.virtualdesktop.agent";
const AGENTS: ActionUuid = "local.virtualdesktop.agents";
const AGENT_PREV: ActionUuid = "local.virtualdesktop.agentprev";
const AGENT_NEXT: ActionUuid = "local.virtualdesktop.agentnext";
const AGENT_REFRESH: ActionUuid = "local.virtualdesktop.agentrefresh";

static VD: OnceLock<vd::Handle> = OnceLock::new();

fn vd() -> &'static vd::Handle {
	VD.get().expect("virtual desktop worker not started")
}

/// Last image sent to each key, so unchanged keys are not redrawn.
static SENT: LazyLock<Mutex<HashMap<InstanceId, String>>> = LazyLock::new(Default::default);
/// One redraw at a time.
static REDRAW: Mutex<()> = Mutex::const_new(());
/// The tool each AI Session key is for, from its settings. openaction only
/// hands settings to the callbacks, so they are kept here.
static LANES: LazyLock<Mutex<HashMap<InstanceId, Option<agents::Tool>>>> = LazyLock::new(Default::default);
/// The page of AI sessions each device shows.
static PAGE: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);
/// When the refresh key was last pressed and how many sessions it removed.
static SWEPT: Mutex<Option<(std::time::Instant, usize)>> = Mutex::const_new(None);
/// How long the refresh key shows what it did.
const SWEPT_SHOWN: Duration = Duration::from_secs(5);

fn lane_of(settings: &serde_json::Value) -> Option<agents::Tool> {
	match settings.get("tool").and_then(serde_json::Value::as_str)?.to_ascii_lowercase().as_str() {
		"claude" => Some(agents::Tool::Claude),
		"codex" => Some(agents::Tool::Codex),
		_ => None,
	}
}

/// Visible keys of one action in reading order, grouped by device.
async fn ordered(action: ActionUuid) -> Vec<Arc<Instance>> {
	let mut keys: Vec<_> = visible_instances(action).await.into_iter().filter(|i| i.coordinates.is_some()).collect();
	keys.sort_by_key(|i| {
		let c = i.coordinates.expect("filtered above");
		(i.device_id.clone(), c.row, c.column)
	});
	keys
}

fn slot_index(slots: &[Arc<Instance>], instance: &Instance) -> Option<usize> {
	slots
		.iter()
		.filter(|s| s.device_id == instance.device_id)
		.position(|s| s.instance_id == instance.instance_id)
}

async fn send(instance: &Instance, svg: String) {
	let url = render::data_url(&svg);
	{
		let mut sent = SENT.lock().await;
		if sent.get(&instance.instance_id) == Some(&url) {
			return;
		}
		sent.insert(instance.instance_id.clone(), url.clone());
	}
	if let Err(e) = instance.set_image(Some(url), None).await {
		log::warn!("set_image failed for {}: {e}", instance.instance_id);
	}
}

async fn forget(instance: &Instance) {
	SENT.lock().await.remove(&instance.instance_id);
}

/// Counts keys appearing, so that only the last of a burst resends.
static APPEARED: AtomicU64 = AtomicU64::new(0);
/// How long after the last key appeared the images are sent again.
const SETTLE: Duration = Duration::from_millis(700);

/// A key appeared: draw, and once the burst of appearing keys (a profile
/// switch) has settled, send every image once more.
///
/// OpenDeck holds its locks for the whole switch and applies queued images
/// afterwards, drawing each key asynchronously in its window process. Images
/// that reach one key in quick succession (what OpenDeck had stored, then
/// ours) can be drawn in the wrong order, leaving the old one on the device
/// while `SENT` says the new one was sent.
async fn appeared() {
	let burst = APPEARED.fetch_add(1, Ordering::Relaxed) + 1;
	tokio::spawn(async move {
		tokio::time::sleep(SETTLE).await;
		if APPEARED.load(Ordering::Relaxed) == burst {
			log::info!("keys appeared: sending all images again");
			redraw(true).await;
		}
	});
	redraw_all().await;
}

fn neighbour(snap: &vd::Snapshot, next: bool) -> Option<(usize, &str)> {
	let index = if next { snap.current.checked_add(1)? } else { snap.current.checked_sub(1)? };
	snap.names.get(index).map(|n| (index, n.as_str()))
}

/// The AI sessions to show, each with its desktop index, in display order:
/// by desktop, then Claude before Codex, then name.
///
/// A session whose desktop is not known (or no longer exists) is left out:
/// its key could not take the user anywhere. It shows up once a prompt or a
/// window tells where it is.
fn agent_view(snap: &vd::Snapshot) -> Vec<(agents::View, usize)> {
	let mut unplaced = Vec::new();
	let mut list: Vec<_> = agents::visible()
		.into_iter()
		.filter_map(|s| match snap.ids.iter().position(|x| Some(x) == s.desktop.as_ref()) {
			Some(index) => Some((s, index)),
			None => {
				unplaced.push(format!("{:?} {}", s.tool, s.label));
				None
			}
		})
		.collect();
	list.sort_by_cached_key(|(s, i)| (*i, s.tool, s.label.clone()));

	// Say which sessions are left out, once each time that changes.
	unplaced.sort();
	let mut last = UNPLACED.lock().unwrap();
	if *last != unplaced {
		log::info!("not shown, desktop unknown: {unplaced:?}");
		*last = unplaced;
	}
	list
}

/// Sessions last left out of `agent_view` for want of a desktop.
static UNPLACED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Where every session goes on one device, page by page.
#[derive(Debug, PartialEq)]
struct Pages {
	/// Per page, the session each key shows (an index into the session list).
	keys: Vec<Vec<Option<usize>>>,
	/// The page each session is on; None when no key can show it.
	page_of: Vec<Option<usize>>,
}

/// Lay the sessions out on one device's AI Session keys.
///
/// `lanes` is the tool of each key, in reading order; `sessions` is (tool,
/// desktop index) of each session, in display order (by desktop). The k-th
/// key of every tool's row makes column k, the Claude row above the Codex
/// row. Each desktop with sessions takes the next column(s): its Claude
/// sessions in the Claude row and its Codex sessions in the Codex row, so one
/// desktop's sessions sit one above the other. Columns run on to further
/// pages, without splitting a desktop across two pages when it fits on one.
/// Keys set to no tool list, in order, the sessions of tools that have no row
/// (all of them when no key is set to a tool).
fn paginate(lanes: &[Option<agents::Tool>], sessions: &[(agents::Tool, usize)]) -> Pages {
	use agents::Tool;
	const TOOLS: [Tool; 2] = [Tool::Claude, Tool::Codex];
	let rows: Vec<Vec<usize>> =
		TOOLS.iter().map(|t| (0..lanes.len()).filter(|&k| lanes[k] == Some(*t)).collect()).collect();
	let spare: Vec<usize> = (0..lanes.len()).filter(|&k| lanes[k].is_none()).collect();
	let width = rows.iter().map(Vec::len).filter(|&n| n > 0).min().unwrap_or(0);
	let row_of = |j: usize| TOOLS.iter().position(|t| *t == sessions[j].0).filter(|&r| !rows[r].is_empty());

	let mut cells: Vec<(usize, usize, usize)> = Vec::new(); // (row, column, session)
	let mut listed: Vec<usize> = Vec::new();
	let mut column = 0;
	let mut start = 0;
	while start < sessions.len() {
		// One desktop's sessions.
		let desktop = sessions[start].1;
		let end = (start..sessions.len()).find(|&j| sessions[j].1 != desktop).unwrap_or(sessions.len());
		let need = (0..TOOLS.len()).map(|r| (start..end).filter(|&j| row_of(j) == Some(r)).count()).max().unwrap_or(0);
		if need > 0 && need <= width && column % width + need > width {
			column += width - column % width; // start this desktop on the next page
		}
		let mut used = [0usize; TOOLS.len()];
		for j in start..end {
			match row_of(j) {
				Some(r) => {
					cells.push((r, column + used[r], j));
					used[r] += 1;
				}
				None => listed.push(j),
			}
		}
		column += need;
		start = end;
	}

	let grid_pages = if width == 0 { 0 } else { column.div_ceil(width) };
	let list_pages = if spare.is_empty() { 0 } else { listed.len().div_ceil(spare.len()) };
	let count = grid_pages.max(list_pages).max(1);
	let mut keys = vec![vec![None; lanes.len()]; count];
	let mut page_of = vec![None; sessions.len()];
	for (r, c, j) in cells {
		keys[c / width][rows[r][c % width]] = Some(j);
		page_of[j] = Some(c / width);
	}
	if !spare.is_empty() {
		for (n, j) in listed.into_iter().enumerate() {
			keys[n / spare.len()][spare[n % spare.len()]] = Some(j);
			page_of[j] = Some(n / spare.len());
		}
	}
	Pages { keys, page_of }
}

/// One device's AI Session keys on the page it is showing.
struct AgentPage {
	/// Each key with the index into the session list it shows.
	keys: Vec<(Arc<Instance>, Option<usize>)>,
	page: usize,
	pages: usize,
	page_of: Vec<Option<usize>>,
}

impl AgentPage {
	/// The page the previous / next key goes to, if there is one.
	fn step(&self, next: bool) -> Option<usize> {
		let to = if next { self.page + 1 } else { self.page.checked_sub(1)? };
		(to < self.pages).then_some(to)
	}

	/// Whether a session on a page that way needs the user.
	fn attention_toward(&self, next: bool, sessions: &[(agents::View, usize)]) -> bool {
		sessions.iter().zip(&self.page_of).any(|((s, _), page)| {
			s.state.needs_user() && page.is_some_and(|p| if next { p > self.page } else { p < self.page })
		})
	}
}

/// The AI Session keys of every device, laid out for the page it is on.
async fn agent_pages(sessions: &[(agents::View, usize)]) -> HashMap<String, AgentPage> {
	let slots = ordered(AGENT).await;
	let placed: Vec<_> = sessions.iter().map(|(s, desktop)| (s.tool, *desktop)).collect();
	let lanes = LANES.lock().await;
	let current = PAGE.lock().await;
	let mut out = HashMap::new();
	for device in slots.chunk_by(|a, b| a.device_id == b.device_id) {
		let device_id = device[0].device_id.clone();
		let device_lanes: Vec<_> = device.iter().map(|i| lanes.get(&i.instance_id).copied().flatten()).collect();
		let Pages { keys, page_of } = paginate(&device_lanes, &placed);
		let pages = keys.len();
		// Sessions may have gone since the page was picked.
		let page = current.get(&device_id).copied().unwrap_or(0).min(pages - 1);
		let keys = device.iter().cloned().zip(keys[page].iter().copied()).collect();
		out.insert(device_id, AgentPage { keys, page, pages, page_of });
	}
	out
}

/// `current` is the desktop being shown: sessions on it get a frame.
fn card(s: &agents::View, desktop: usize, current: usize, now: u64) -> render::AgentCard<'_> {
	render::AgentCard {
		tool: s.tool,
		name: &s.label,
		state: s.state,
		stale: s.stale,
		desktop,
		elapsed_ms: now.saturating_sub(s.since),
		here: desktop == current,
	}
}

async fn redraw_all() {
	redraw(false).await;
}

/// Draw every visible key. `resend` sends images even where nothing changed.
async fn redraw(resend: bool) {
	let _guard = REDRAW.lock().await;
	if resend {
		SENT.lock().await.clear();
	}
	let snap = vd().snapshot();

	let mut seen_per_device: HashMap<String, usize> = HashMap::new();
	for slot in ordered(SLOT).await {
		let counter = seen_per_device.entry(slot.device_id.clone()).or_insert(0);
		let index = *counter;
		*counter += 1;
		let svg = match snap.names.get(index) {
			Some(name) => render::slot(index, name, index == snap.current),
			None => render::empty(),
		};
		send(&slot, svg).await;
	}

	let current = snap.names.get(snap.current).map(|n| (snap.current, n.as_str()));
	for key in visible_instances(INFO).await {
		send(&key, render::info(current, snap.count())).await;
	}
	for key in visible_instances(PREV).await {
		send(&key, render::arrow(false, neighbour(&snap, false))).await;
	}
	for key in visible_instances(NEXT).await {
		send(&key, render::arrow(true, neighbour(&snap, true))).await;
	}

	let now = agents::now_ms();
	let sessions = agent_view(&snap);
	let pages = agent_pages(&sessions).await;
	let setup = setup_open().await;
	if setup {
		// The summary key's settings take over the session keys.
		let prefs = prefs::get();
		for (slot, option) in setup_keys().await {
			let svg = option.map_or_else(render::empty, |tool| render::jump_option(tool, prefs.jumps_to(tool)));
			send(&slot, svg).await;
		}
	} else {
		for (slot, pick) in pages.values().flat_map(|p| &p.keys) {
			let svg = match pick.map(|i| &sessions[i]) {
				Some((s, desktop)) => render::agent(&card(s, *desktop, snap.current, now)),
				None => render::empty(),
			};
			send(slot, svg).await;
		}
	}
	for next in [false, true] {
		for key in visible_instances(if next { AGENT_NEXT } else { AGENT_PREV }).await {
			let page = pages.get(&key.device_id);
			let target = page.and_then(|p| Some((p.step(next)?, p.pages)));
			let attention = page.is_some_and(|p| p.attention_toward(next, &sessions));
			send(&key, render::page_arrow(next, target, attention)).await;
		}
	}
	let swept = SWEPT.lock().await.filter(|(at, _)| at.elapsed() < SWEPT_SHOWN).map(|(_, removed)| removed);
	for key in visible_instances(AGENT_REFRESH).await {
		send(&key, render::refresh(swept)).await;
	}
	let summary_keys = visible_instances(AGENTS).await;
	if !summary_keys.is_empty() {
		let count = |f: &dyn Fn(&agents::View) -> bool| sessions.iter().filter(|(s, _)| f(s)).count();
		let attention = count(&|s| s.state.needs_user());
		let done = count(&|s| s.state == agents::State::Done);
		let working = count(&|s| matches!(s.state, agents::State::Working | agents::State::Reviewing) && !s.stale);
		for key in summary_keys {
			let svg = if setup { render::settings_close() } else { render::agents_summary(attention, done, working) };
			send(&key, svg).await;
		}
	}
}

// ---- the summary key's settings screen
//
// Holding the summary key turns the AI Session keys into checkboxes: which
// tools' sessions the summary key goes to. A plugin cannot switch profiles in
// OpenDeck, so the screen is drawn over the keys that are there.

/// When the settings screen was last touched; None while it is closed.
static SETUP: Mutex<Option<std::time::Instant>> = Mutex::const_new(None);
/// The settings screen closes by itself after this long without a press.
const SETUP_IDLE: Duration = Duration::from_secs(20);
/// Holding the summary key this long opens the settings screen.
const LONG_PRESS: Duration = Duration::from_millis(600);
/// Counts presses of the summary key.
static PRESSES: AtomicU64 = AtomicU64::new(0);
/// The press of the summary key that is still held down and has not yet
/// turned into a long press.
static HELD: Mutex<Option<u64>> = Mutex::const_new(None);

async fn setup_open() -> bool {
	let mut setup = SETUP.lock().await;
	if setup.is_some_and(|touched| touched.elapsed() > SETUP_IDLE) {
		*setup = None;
	}
	setup.is_some()
}

/// Which of one device's AI Session keys (their tools, in reading order) hold
/// the Claude and the Codex checkbox: the first key of that tool's row, or
/// failing that the next key set to no tool.
fn option_keys(lanes: &[Option<agents::Tool>]) -> [(agents::Tool, Option<usize>); 2] {
	let mut spare = (0..lanes.len()).filter(|&k| lanes[k].is_none());
	[agents::Tool::Claude, agents::Tool::Codex].map(|tool| (tool, lanes.iter().position(|l| *l == Some(tool)).or_else(|| spare.next())))
}

/// Every visible AI Session key with the checkbox it shows in the settings
/// screen, if any.
async fn setup_keys() -> Vec<(Arc<Instance>, Option<agents::Tool>)> {
	let slots = ordered(AGENT).await;
	let lanes = LANES.lock().await;
	let mut out = Vec::with_capacity(slots.len());
	for device in slots.chunk_by(|a, b| a.device_id == b.device_id) {
		let device_lanes: Vec<_> = device.iter().map(|i| lanes.get(&i.instance_id).copied().flatten()).collect();
		let options = option_keys(&device_lanes);
		for (k, slot) in device.iter().enumerate() {
			out.push((slot.clone(), options.iter().find(|(_, key)| *key == Some(k)).map(|(tool, _)| *tool)));
		}
	}
	out
}

fn describe(instance: &Instance) -> String {
	match instance.coordinates {
		Some(c) => format!("{} at row {} col {}", instance.instance_id, c.row, c.column),
		None => format!("{} (no position)", instance.instance_id),
	}
}

struct SlotAction;
#[async_trait]
impl Action for SlotAction {
	const UUID: ActionUuid = SLOT;
	type Settings = serde_json::Value;

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		log::info!("slot appeared: {}", describe(instance));
		appeared().await;
		Ok(())
	}

	/// No redraw here. On a profile switch the slots go one by one, and
	/// renumbering the rest each time would send every one of them a "1"
	/// just before it goes, to be drawn over the right image on the way
	/// back. A slot really removed is picked up by the periodic redraw.
	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let slots = ordered(SLOT).await;
		match slot_index(&slots, instance) {
			Some(i) if i < vd().snapshot().count() => vd().switch(vd::Target::Index(i)),
			_ => instance.show_alert().await?,
		}
		Ok(())
	}
}

struct InfoAction;
#[async_trait]
impl Action for InfoAction {
	const UUID: ActionUuid = INFO;
	type Settings = serde_json::Value;

	async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		appeared().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		vd().refresh();
		instance.show_ok().await
	}
}

struct StepAction<const NEXT_DIR: bool>;
#[async_trait]
impl<const NEXT_DIR: bool> Action for StepAction<NEXT_DIR> {
	const UUID: ActionUuid = if NEXT_DIR { NEXT } else { PREV };
	type Settings = serde_json::Value;

	async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		appeared().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		Ok(())
	}

	async fn key_up(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		vd().switch(if NEXT_DIR { vd::Target::Next } else { vd::Target::Prev });
		Ok(())
	}
}

struct AgentAction;
#[async_trait]
impl Action for AgentAction {
	const UUID: ActionUuid = AGENT;
	type Settings = serde_json::Value;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		let lane = lane_of(settings);
		log::info!("agent slot appeared: {} for {lane:?}", describe(instance));
		LANES.lock().await.insert(instance.instance_id.clone(), lane);
		appeared().await;
		Ok(())
	}

	async fn did_receive_settings(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		LANES.lock().await.insert(instance.instance_id.clone(), lane_of(settings));
		redraw_all().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		LANES.lock().await.remove(&instance.instance_id);
		forget(instance).await;
		Ok(())
	}

	/// Go to the desktop the session is on. An empty slot does nothing. In
	/// the settings screen, tick or clear the key's checkbox instead.
	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		if setup_open().await {
			let option = setup_keys().await.into_iter().find(|(slot, _)| slot.instance_id == instance.instance_id);
			if let Some((_, Some(tool))) = option {
				let prefs = prefs::toggle_jump(tool);
				log::info!("summary key now goes to: Claude {}, Codex {}", prefs.jump_claude, prefs.jump_codex);
				*SETUP.lock().await = Some(std::time::Instant::now());
				redraw_all().await;
			}
			return Ok(());
		}
		let sessions = agent_view(&vd().snapshot());
		let pages = agent_pages(&sessions).await;
		let pick = pages
			.get(&instance.device_id)
			.and_then(|p| p.keys.iter().find(|(slot, _)| slot.instance_id == instance.instance_id))
			.and_then(|(_, pick)| *pick);
		if let Some(i) = pick {
			vd().switch(vd::Target::Index(sessions[i].1));
		}
		Ok(())
	}
}

/// Previous / next page of AI sessions, for when they do not fit on one.
struct PageAction<const NEXT_DIR: bool>;
#[async_trait]
impl<const NEXT_DIR: bool> Action for PageAction<NEXT_DIR> {
	const UUID: ActionUuid = if NEXT_DIR { AGENT_NEXT } else { AGENT_PREV };
	type Settings = serde_json::Value;

	async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		appeared().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let sessions = agent_view(&vd().snapshot());
		let pages = agent_pages(&sessions).await;
		let Some(page) = pages.get(&instance.device_id) else { return Ok(()) };
		if let Some(to) = page.step(NEXT_DIR) {
			log::info!("AI page {} / {}", to + 1, page.pages);
			PAGE.lock().await.insert(instance.device_id.clone(), to);
			redraw_all().await;
		}
		Ok(())
	}
}

/// Re-read everything now and clear away sessions that look closed.
struct RefreshAction;
#[async_trait]
impl Action for RefreshAction {
	const UUID: ActionUuid = AGENT_REFRESH;
	type Settings = serde_json::Value;

	async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		appeared().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		Ok(())
	}

	async fn key_up(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		vd().refresh();
		let removed = agents::sweep().await;
		log::info!("refresh: {removed} closed session(s) removed");
		*SWEPT.lock().await = Some((std::time::Instant::now(), removed));
		redraw(true).await;
		// Put the key's own face back once the result has been shown.
		tokio::spawn(async {
			tokio::time::sleep(SWEPT_SHOWN).await;
			redraw_all().await;
		});
		Ok(())
	}
}

struct AgentsSummaryAction;
#[async_trait]
impl Action for AgentsSummaryAction {
	const UUID: ActionUuid = AGENTS;
	type Settings = serde_json::Value;

	async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		appeared().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		*SETUP.lock().await = None; // leaving the profile closes the settings screen
		Ok(())
	}

	/// Held long enough, the key opens its settings screen (without waiting
	/// for the release).
	async fn key_down(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let press = PRESSES.fetch_add(1, Ordering::Relaxed) + 1;
		*HELD.lock().await = Some(press);
		tokio::spawn(async move {
			tokio::time::sleep(LONG_PRESS).await;
			{
				let mut held = HELD.lock().await;
				if *held != Some(press) {
					return; // released, or pressed again, in the meantime
				}
				*held = None; // the release that follows is not a short press
			}
			log::info!("summary key held: settings screen");
			*SETUP.lock().await = Some(std::time::Instant::now());
			redraw_all().await;
		});
		Ok(())
	}

	/// A short press. Go to the session that most needs the user; pressed
	/// again soon after, to the next one, and round again after the last.
	/// With the settings screen open, close it.
	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		if HELD.lock().await.take().is_none() {
			return Ok(()); // the end of a long press
		}
		if setup_open().await {
			*SETUP.lock().await = None;
			redraw_all().await;
			return Ok(());
		}
		let sessions = agent_view(&vd().snapshot());
		let stops = tour_stops(&sessions, prefs::get());
		let mut tour = TOUR.lock().await;
		if stops.is_empty() {
			*tour = None;
			drop(tour);
			// nothing is waiting for the user: say so instead of an error mark
			return instance.show_ok().await;
		}
		let last = tour.as_ref().filter(|t| t.pressed.elapsed() < TOUR_KEEP).map(|t| (t.stops.as_slice(), t.at));
		let at = tour_step(last, &stops);
		let desktop = stops[at];
		log::info!("summary key: stop {} of {}, desktop {}", at + 1, stops.len(), desktop + 1);
		vd().switch(vd::Target::Index(desktop));
		*tour = Some(Tour { stops, at, pressed: std::time::Instant::now() });
		drop(tour);

		// Turn the deck to the page that shows that desktop's sessions.
		let pages = agent_pages(&sessions).await;
		if let Some(page) = pages.get(&instance.device_id)
			&& let Some(to) = page_showing(desktop, &sessions, &page.page_of)
			&& to != page.page
		{
			log::info!("AI page {} / {} (following the summary key)", to + 1, page.pages);
			PAGE.lock().await.insert(instance.device_id.clone(), to);
			redraw_all().await;
		}
		Ok(())
	}
}

/// The page of AI Session keys that shows the sessions on `desktop`.
/// `page_of` is the page of each of `sessions` (see `Pages`).
fn page_showing(desktop: usize, sessions: &[(agents::View, usize)], page_of: &[Option<usize>]) -> Option<usize> {
	sessions.iter().zip(page_of).find_map(|((_, on), page)| if *on == desktop { *page } else { None })
}

/// Where the summary key has been taking the user.
struct Tour {
	/// Desktops to visit, as they were at the last press.
	stops: Vec<usize>,
	/// Index in `stops` of the one last gone to.
	at: usize,
	pressed: std::time::Instant,
}

static TOUR: Mutex<Option<Tour>> = Mutex::const_new(None);
/// Pressing the summary key again within this time goes on to the next stop.
const TOUR_KEEP: Duration = Duration::from_secs(10);

/// The desktops with a session waiting for the user, in the order the summary
/// key visits them: approval first, then input, error, answered; within one
/// kind, the longest wait first. Each desktop once. Only sessions of the tools
/// ticked in the key's settings count.
fn tour_stops(sessions: &[(agents::View, usize)], prefs: prefs::Prefs) -> Vec<usize> {
	let mut waiting: Vec<(u8, u64, usize)> = sessions
		.iter()
		.filter(|(s, _)| prefs.jumps_to(s.tool) && s.state.urgency() <= agents::State::Done.urgency())
		.map(|(s, desktop)| (s.state.urgency(), s.since, *desktop))
		.collect();
	waiting.sort_unstable();
	let mut stops = Vec::new();
	for (_, _, desktop) in waiting {
		if !stops.contains(&desktop) {
			stops.push(desktop);
		}
	}
	stops
}

/// Which of `stops` (not empty) to go to. `last` is the stops and position of
/// a recent press: with the same stops, go on to the next and wrap round;
/// otherwise start from the first.
fn tour_step(last: Option<(&[usize], usize)>, stops: &[usize]) -> usize {
	match last {
		Some((before, at)) if before == stops => (at + 1) % stops.len(),
		_ => 0,
	}
}

fn init_logging(to_file: bool) {
	use simplelog::*;
	let config = ConfigBuilder::new().set_time_offset_to_local().unwrap_or_else(|b| b).build();
	// Keep the previous run's log: a restart (deploy) would otherwise wipe
	// what led up to it.
	let file = to_file
		.then(|| std::env::current_exe().ok().map(|p| p.with_file_name("vdesk.log")))
		.flatten()
		.and_then(|p| {
			let _ = std::fs::rename(&p, p.with_file_name("vdesk.prev.log"));
			std::fs::File::create(p).ok()
		});
	let _ = match file {
		Some(f) => WriteLogger::init(LevelFilter::Info, config, f),
		None => TermLogger::init(LevelFilter::Info, config, TerminalMode::Stdout, ColorChoice::Never),
	};
}

fn preview_grid(title: &str, cells: &[String]) -> String {
	let imgs: String = cells.iter().map(|s| format!(r#"<img src="{}">"#, render::data_url(s))).collect();
	format!(r#"<h2>{title}</h2><div>{imgs}</div>"#)
}

/// Read-only check: what winvd sees, a no-op switch to the current desktop,
/// and a preview of both key layouts (the AI one with made-up sessions).
fn selftest() -> std::process::ExitCode {
	let snap = match vd::read() {
		Ok(s) => s,
		Err(e) => {
			println!("FAILED to read desktops: {e:?}");
			return std::process::ExitCode::FAILURE;
		}
	};
	println!("desktops: {}", snap.count());
	for (i, name) in snap.names.iter().enumerate() {
		println!("  {}{}: {:?}", i + 1, if i == snap.current { " *" } else { "" }, name);
	}
	match winvd::switch_desktop(snap.current as u32) {
		Ok(()) => println!("switch call (to the current desktop, no visible change): ok"),
		Err(e) => println!("switch call FAILED: {e:?} (keyboard fallback would be used)"),
	}
	println!("claude windows:");
	for w in winutil::app_windows(&["claude"]) {
		let index = w.desktop.as_ref().and_then(|g| snap.ids.iter().position(|x| x == g));
		println!("  desktop {:?}: {:?}", index.map(|i| i + 1), w.title);
	}

	// Desktop profile: 10 slots, then prev / info / next.
	let current = snap.names.get(snap.current).map(|n| (snap.current, n.as_str()));
	let mut desk: Vec<String> = (0..10)
		.map(|i| match snap.names.get(i) {
			Some(n) => render::slot(i, n, i == snap.current),
			None => render::empty(),
		})
		.collect();
	desk.push(render::arrow(false, neighbour(&snap, false)));
	desk.push(render::info(current, snap.count()));
	desk.push(render::arrow(true, neighbour(&snap, true)));
	desk.push(render::empty());
	desk.push(render::empty());

	// AI profile with sample sessions (one of each state) in display order,
	// enough desktops for a second page.
	use agents::{State, Tool};
	// As if desktop 3 were showing.
	let sample = |tool, name, state, desktop, elapsed_ms| render::AgentCard {
		tool,
		name,
		state,
		stale: false,
		desktop,
		elapsed_ms,
		here: desktop == 2,
	};
	let samples = [
		sample(Tool::Claude, "Unity", State::Working, 1, 60_000),
		sample(Tool::Codex, "Unity", State::Reviewing, 1, 4_000),
		sample(Tool::Claude, "GMK01", State::Working, 2, 3 * 60_000),
		sample(Tool::Codex, "MatterMost", State::Done, 2, 12 * 60_000),
		sample(Tool::Claude, "GMK02", State::Done, 3, 40_000),
		sample(Tool::Claude, "Fujitu01", State::Done, 4, 2 * 3_600_000),
		sample(Tool::Codex, "見積もり整理レセコン", State::Input, 4, 5 * 60_000),
		render::AgentCard { stale: true, ..sample(Tool::Claude, "Epson01", State::Working, 5, 20 * 60_000) },
		sample(Tool::Codex, "DoveBar", State::Permission, 6, 40_000),
	];
	// Same as layout/AI.json: Claude row, Codex row, then
	// [→Default][refresh][◀][summary][▶].
	let lanes: Vec<Option<Tool>> = [Some(Tool::Claude); 5].into_iter().chain([Some(Tool::Codex); 5]).collect();
	let placed: Vec<(Tool, usize)> = samples.iter().map(|s| (s.tool, s.desktop)).collect();
	let Pages { keys, page_of } = paginate(&lanes, &placed);
	let pages = keys.len();
	let mut grids = String::new();
	for (page, picks) in keys.iter().enumerate() {
		let mut ai: Vec<String> =
			picks.iter().map(|pick| pick.map_or_else(render::empty, |i| render::agent(&samples[i]))).collect();
		let attention = |next: bool| {
			samples.iter().zip(&page_of).any(|(s, p)| {
				s.state.needs_user() && p.is_some_and(|p| if next { p > page } else { p < page })
			})
		};
		ai.push(render::empty()); // the Starter Pack switch to Default
		ai.push(render::refresh((page > 0).then_some(2)));
		ai.push(render::page_arrow(false, page.checked_sub(1).map(|p| (p, pages)), attention(false)));
		ai.push(render::agents_summary(2, 2, 2));
		ai.push(render::page_arrow(true, (page + 1 < pages).then_some((page + 1, pages)), attention(true)));
		grids.push_str(&preview_grid(&format!("AI page {}/{pages} (sample data)", page + 1), &ai));
	}

	// The summary key's settings screen, with Codex cleared.
	let mut setup: Vec<String> = (0..10)
		.map(|k| match k {
			0 => render::jump_option(Tool::Claude, true),
			5 => render::jump_option(Tool::Codex, false),
			_ => render::empty(),
		})
		.collect();
	setup.extend([
		render::empty(),
		render::refresh(None),
		render::page_arrow(false, None, false),
		render::settings_close(),
		render::page_arrow(true, None, false),
	]);
	grids.push_str(&preview_grid("AI: summary key held (settings)", &setup));

	let html = format!(
		r#"<!doctype html><meta charset="utf-8"><title>vdesk preview</title><style>body{{background:#111;color:#ccc;font-family:sans-serif;margin:24px}}div{{display:grid;grid-template-columns:repeat(5,96px);gap:12px;margin-bottom:24px}}img{{width:96px;height:96px;border-radius:12px}}h2{{font-size:14px}}</style>{}{grids}"#,
		preview_grid("Default", &desk)
	);
	let path = std::env::current_dir().unwrap_or_default().join("preview.html");
	match std::fs::write(&path, html) {
		Ok(()) => println!("preview: {}", path.display()),
		Err(e) => println!("could not write preview: {e}"),
	}
	std::process::ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
	let args: Vec<String> = std::env::args().collect();
	if args.iter().any(|a| a == "--selftest") {
		init_logging(false);
		return selftest();
	}

	init_logging(true);
	log::info!("vdesk {} starting", env!("CARGO_PKG_VERSION"));
	let _ = VD.set(vd::start());
	front::start();

	register_action(SlotAction).await;
	register_action(InfoAction).await;
	register_action(StepAction::<false>).await;
	register_action(StepAction::<true>).await;
	register_action(AgentAction).await;
	register_action(AgentsSummaryAction).await;
	register_action(PageAction::<false>).await;
	register_action(PageAction::<true>).await;
	register_action(RefreshAction).await;

	tokio::spawn(agents::serve());
	tokio::spawn(async {
		let mut changes = vd().subscribe();
		while changes.changed().await.is_ok() {
			redraw_all().await;
		}
	});
	tokio::spawn(async {
		let mut changes = agents::subscribe();
		while changes.changed().await.is_ok() {
			redraw_all().await;
		}
	});
	// Names, window positions, "minutes ago" and stale sessions.
	tokio::spawn(async {
		let mut tick = tokio::time::interval(Duration::from_secs(3));
		loop {
			tick.tick().await;
			agents::maintain().await;
			redraw_all().await;
		}
	});

	match run(args).await {
		Ok(()) => std::process::ExitCode::SUCCESS,
		Err(e) => {
			log::error!("plugin stopped: {e}");
			std::process::ExitCode::FAILURE
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use agents::Tool::{Claude, Codex};

	/// 3 columns: a Claude row over a Codex row.
	const LANES3: [Option<agents::Tool>; 6] = [Some(Claude), Some(Claude), Some(Claude), Some(Codex), Some(Codex), Some(Codex)];

	fn keys(lanes: &[Option<agents::Tool>], sessions: &[(agents::Tool, usize)]) -> Vec<Vec<Option<usize>>> {
		paginate(lanes, sessions).keys
	}

	#[test]
	fn same_desktop_sits_in_one_column() {
		// desktop 0: Claude only; desktop 2: Claude + Codex; desktop 4: Codex only
		let sessions = [(Claude, 0), (Claude, 2), (Codex, 2), (Codex, 4)];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), Some(1), None, None, Some(2), Some(3)]]);
	}

	#[test]
	fn a_desktop_with_two_of_one_tool_takes_two_columns() {
		let sessions = [(Claude, 1), (Claude, 1), (Codex, 1), (Claude, 3)];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), Some(1), Some(3), Some(2), None, None]]);
	}

	#[test]
	fn columns_run_on_to_the_next_page_without_splitting_a_desktop() {
		// desktop 2 needs 2 columns but only 1 is left on page 1
		let sessions = [(Claude, 0), (Claude, 1), (Claude, 2), (Claude, 2), (Codex, 2)];
		let pages = paginate(&LANES3, &sessions);
		assert_eq!(
			pages.keys,
			vec![vec![Some(0), Some(1), None, None, None, None], vec![Some(2), Some(3), None, Some(4), None, None]]
		);
		assert_eq!(pages.page_of, vec![Some(0), Some(0), Some(1), Some(1), Some(1)]);
	}

	#[test]
	fn a_tool_without_a_row_goes_to_keys_for_no_tool() {
		let lanes = [Some(Claude), Some(Claude), None];
		assert_eq!(keys(&lanes, &[(Codex, 0), (Claude, 0)]), vec![vec![Some(1), None, Some(0)]]);
	}

	#[test]
	fn keys_for_no_tool_list_everything_in_pages() {
		let sessions = [(Claude, 0), (Codex, 0), (Claude, 1)];
		assert_eq!(keys(&[None, None], &sessions), vec![vec![Some(0), Some(1)], vec![Some(2), None]]);
	}

	fn view(state: agents::State, since: u64) -> agents::View {
		agents::View { tool: Claude, label: String::new(), state, stale: false, since, desktop: None }
	}

	#[test]
	fn summary_key_visits_the_most_urgent_first_and_each_desktop_once() {
		use agents::State::*;
		let sessions = [
			(view(Done, 10), 4),
			(view(Working, 1), 0),    // needs nobody
			(view(Permission, 50), 2),
			(view(Permission, 40), 5), // waiting longer than the one on 2
			(view(Done, 20), 2),       // desktop 2 is already a stop
			(view(Error, 30), 1),
			(view(Idle, 1), 3),
		];
		assert_eq!(tour_stops(&sessions, prefs::Prefs::default()), vec![5, 2, 1, 4]);
	}

	#[test]
	fn summary_key_turns_the_deck_to_the_page_of_the_desktop_it_goes_to() {
		use agents::State::*;
		// 3 columns a page: desktops 0, 1, 2 on page 1, desktop 5 on page 2
		let sessions = [(view(Done, 1), 0), (view(Done, 1), 1), (view(Done, 1), 2), (view(Permission, 1), 5)];
		let placed: Vec<_> = sessions.iter().map(|(s, d)| (s.tool, *d)).collect();
		let pages = paginate(&LANES3, &placed);
		assert_eq!(page_showing(5, &sessions, &pages.page_of), Some(1));
		assert_eq!(page_showing(1, &sessions, &pages.page_of), Some(0));
		assert_eq!(page_showing(4, &sessions, &pages.page_of), None); // nothing shown for it
	}

	#[test]
	fn summary_key_skips_tools_not_ticked_in_its_settings() {
		use agents::State::*;
		let codex = |state, since| agents::View { tool: Codex, ..view(state, since) };
		let sessions = [(view(Done, 10), 4), (codex(Permission, 5), 2), (codex(Done, 1), 4)];
		let only_claude = prefs::Prefs { jump_claude: true, jump_codex: false };
		assert_eq!(tour_stops(&sessions, only_claude), vec![4]);
		assert_eq!(tour_stops(&sessions, prefs::Prefs { jump_claude: false, jump_codex: true }), vec![2, 4]);
		assert!(tour_stops(&sessions, prefs::Prefs { jump_claude: false, jump_codex: false }).is_empty());
	}

	#[test]
	fn settings_checkboxes_sit_on_the_first_key_of_each_row() {
		let at = |lanes: &[Option<agents::Tool>]| option_keys(lanes).map(|(_, key)| key);
		assert_eq!(at(&LANES3), [Some(0), Some(3)]);
		// no rows: the first two keys
		assert_eq!(at(&[None, None, None]), [Some(0), Some(1)]);
		// a Claude row only: Codex takes the first key set to no tool
		assert_eq!(at(&[Some(Claude), Some(Claude), None]), [Some(0), Some(2)]);
		assert_eq!(at(&[Some(Claude)]), [Some(0), None]);
	}

	#[test]
	fn summary_key_goes_on_to_the_next_stop_and_round_again() {
		let stops = [5, 2, 1];
		assert_eq!(tour_step(None, &stops), 0);
		assert_eq!(tour_step(Some((&stops, 0)), &stops), 1);
		assert_eq!(tour_step(Some((&stops, 2)), &stops), 0);
		// the stops changed since the last press: from the top
		assert_eq!(tour_step(Some((&[5, 2], 0)), &stops), 0);
	}

	#[test]
	fn no_sessions_is_one_empty_page() {
		assert_eq!(keys(&LANES3, &[]), vec![vec![None; 6]]);
	}
}
