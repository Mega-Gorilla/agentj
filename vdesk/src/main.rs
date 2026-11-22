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
//!   go to the session that needs you most.
//!
//! `vdesk.exe --selftest` prints what it reads from Windows and writes
//! preview.html, without switching anything.

mod agents;
mod keys;
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

/// Visible AI sessions with their desktop index, in display order:
/// by desktop, then Claude before Codex, then name.
fn agent_view(snap: &vd::Snapshot) -> Vec<(agents::View, Option<usize>)> {
	let mut list: Vec<_> = agents::visible()
		.into_iter()
		.map(|s| {
			let index = s.desktop.as_ref().and_then(|g| snap.ids.iter().position(|x| x == g));
			(s, index)
		})
		.collect();
	list.sort_by_cached_key(|(s, i)| (i.unwrap_or(usize::MAX), s.tool, s.label.clone()));
	list
}

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
fn paginate(lanes: &[Option<agents::Tool>], sessions: &[(agents::Tool, Option<usize>)]) -> Pages {
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
		// One desktop's sessions; each session on an unknown desktop stands alone.
		let desktop = sessions[start].1;
		let end = match desktop {
			Some(_) => (start..sessions.len()).find(|&j| sessions[j].1 != desktop).unwrap_or(sessions.len()),
			None => start + 1,
		};
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
	fn attention_toward(&self, next: bool, sessions: &[(agents::View, Option<usize>)]) -> bool {
		sessions.iter().zip(&self.page_of).any(|((s, _), page)| {
			s.state.needs_user() && page.is_some_and(|p| if next { p > self.page } else { p < self.page })
		})
	}
}

/// The AI Session keys of every device, laid out for the page it is on.
async fn agent_pages(sessions: &[(agents::View, Option<usize>)]) -> HashMap<String, AgentPage> {
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

fn card(s: &agents::View, desktop: Option<usize>, now: u64) -> render::AgentCard<'_> {
	render::AgentCard {
		tool: s.tool,
		name: &s.label,
		state: s.state,
		stale: s.stale,
		desktop,
		elapsed_ms: now.saturating_sub(s.since),
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
	for (slot, pick) in pages.values().flat_map(|p| &p.keys) {
		let svg = match pick.map(|i| &sessions[i]) {
			Some((s, desktop)) => render::agent(&card(s, *desktop, now)),
			None => render::empty(),
		};
		send(slot, svg).await;
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
			send(&key, render::agents_summary(attention, done, working)).await;
		}
	}
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

	/// Go to the desktop the session is on. A session whose desktop is not
	/// known yet (shown as "?") gets an alert; an empty slot does nothing.
	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let sessions = agent_view(&vd().snapshot());
		let pages = agent_pages(&sessions).await;
		let pick = pages
			.get(&instance.device_id)
			.and_then(|p| p.keys.iter().find(|(slot, _)| slot.instance_id == instance.instance_id))
			.and_then(|(_, pick)| *pick);
		match pick.map(|i| &sessions[i]) {
			Some((_, Some(desktop))) => vd().switch(vd::Target::Index(*desktop)),
			Some((s, None)) => {
				log::info!("{:?} {}: desktop not known yet", s.tool, s.label);
				instance.show_alert().await?
			}
			None => {}
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
		Ok(())
	}

	/// Go to the session that has been waiting for the user the longest.
	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let sessions = agent_view(&vd().snapshot());
		let target = sessions
			.iter()
			.filter(|(s, d)| d.is_some() && s.state.urgency() <= agents::State::Done.urgency())
			.min_by_key(|(s, _)| (s.state.urgency(), s.since));
		match target {
			Some((_, Some(desktop))) => vd().switch(vd::Target::Index(*desktop)),
			// nothing is waiting for the user: say so instead of an error mark
			_ => instance.show_ok().await?,
		}
		Ok(())
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
	let sample = |tool, name, state, desktop, elapsed_ms| render::AgentCard { tool, name, state, stale: false, desktop, elapsed_ms };
	let samples = [
		sample(Tool::Claude, "Unity", State::Working, Some(1), 60_000),
		sample(Tool::Codex, "Unity", State::Reviewing, Some(1), 4_000),
		sample(Tool::Claude, "GMK01", State::Working, Some(2), 3 * 60_000),
		sample(Tool::Codex, "MatterMost", State::Done, Some(2), 12 * 60_000),
		sample(Tool::Claude, "GMK02", State::Done, Some(3), 40_000),
		sample(Tool::Claude, "Fujitu01", State::Done, Some(4), 2 * 3_600_000),
		sample(Tool::Codex, "見積もり整理レセコン", State::Input, Some(4), 5 * 60_000),
		render::AgentCard { stale: true, ..sample(Tool::Claude, "Epson01", State::Working, Some(5), 20 * 60_000) },
		sample(Tool::Codex, "DoveBar", State::Permission, None, 40_000),
	];
	// Same as layout/AI.json: Claude row, Codex row, then
	// [→Default][refresh][◀][summary][▶].
	let lanes: Vec<Option<Tool>> = [Some(Tool::Claude); 5].into_iter().chain([Some(Tool::Codex); 5]).collect();
	let placed: Vec<(Tool, Option<usize>)> = samples.iter().map(|s| (s.tool, s.desktop)).collect();
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

	fn keys(lanes: &[Option<agents::Tool>], sessions: &[(agents::Tool, Option<usize>)]) -> Vec<Vec<Option<usize>>> {
		paginate(lanes, sessions).keys
	}

	#[test]
	fn same_desktop_sits_in_one_column() {
		// desktop 0: Claude only; desktop 2: Claude + Codex; desktop 4: Codex only
		let sessions = [(Claude, Some(0)), (Claude, Some(2)), (Codex, Some(2)), (Codex, Some(4))];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), Some(1), None, None, Some(2), Some(3)]]);
	}

	#[test]
	fn a_desktop_with_two_of_one_tool_takes_two_columns() {
		let sessions = [(Claude, Some(1)), (Claude, Some(1)), (Codex, Some(1)), (Claude, Some(3))];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), Some(1), Some(3), Some(2), None, None]]);
	}

	#[test]
	fn columns_run_on_to_the_next_page_without_splitting_a_desktop() {
		// desktop 2 needs 2 columns but only 1 is left on page 1
		let sessions = [(Claude, Some(0)), (Claude, Some(1)), (Claude, Some(2)), (Claude, Some(2)), (Codex, Some(2))];
		let pages = paginate(&LANES3, &sessions);
		assert_eq!(
			pages.keys,
			vec![vec![Some(0), Some(1), None, None, None, None], vec![Some(2), Some(3), None, Some(4), None, None]]
		);
		assert_eq!(pages.page_of, vec![Some(0), Some(0), Some(1), Some(1), Some(1)]);
	}

	#[test]
	fn sessions_on_unknown_desktops_stand_alone() {
		let sessions = [(Claude, None), (Codex, None)];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), None, None, None, Some(1), None]]);
	}

	#[test]
	fn a_tool_without_a_row_goes_to_keys_for_no_tool() {
		let lanes = [Some(Claude), Some(Claude), None];
		assert_eq!(keys(&lanes, &[(Codex, Some(0)), (Claude, Some(0))]), vec![vec![Some(1), None, Some(0)]]);
	}

	#[test]
	fn keys_for_no_tool_list_everything_in_pages() {
		let sessions = [(Claude, Some(0)), (Codex, Some(0)), (Claude, Some(1))];
		assert_eq!(keys(&[None, None], &sessions), vec![vec![Some(0), Some(1)], vec![Some(2), None]]);
	}

	#[test]
	fn no_sessions_is_one_empty_page() {
		assert_eq!(keys(&LANES3, &[]), vec![vec![None; 6]]);
	}
}
