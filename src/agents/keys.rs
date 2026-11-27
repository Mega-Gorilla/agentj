//! The AI keys:
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
//!   Hold it for its settings screen.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use openaction::*;
use tokio::sync::Mutex;

use super::layout::{Pages, option_keys, page_showing, paginate, tour_step, tour_stops};
use super::render::{self, AgentCard};
use super::{Detail, State, Tool, View, prefs};
use crate::deck::{appeared, describe, forget, ordered, redraw, redraw_all, send};
use crate::desktops::{self, Snapshot, Target};

const AGENT: ActionUuid = "io.github.mega-gorilla.agentj.agent";
const AGENTS: ActionUuid = "io.github.mega-gorilla.agentj.agents";
const AGENT_PREV: ActionUuid = "io.github.mega-gorilla.agentj.agentprev";
const AGENT_NEXT: ActionUuid = "io.github.mega-gorilla.agentj.agentnext";
const AGENT_REFRESH: ActionUuid = "io.github.mega-gorilla.agentj.agentrefresh";

pub async fn register() {
	register_action(AgentAction).await;
	register_action(AgentsSummaryAction).await;
	register_action(PageAction::<false>).await;
	register_action(PageAction::<true>).await;
	register_action(RefreshAction).await;
}

/// The tool each AI Session key is for, from its settings. openaction only
/// hands settings to the callbacks, so they are kept here.
static LANES: LazyLock<Mutex<HashMap<InstanceId, Option<Tool>>>> = LazyLock::new(Default::default);
/// The page of AI sessions each device shows.
static PAGE: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(Default::default);
/// When the refresh key was last pressed and how many sessions it removed.
static SWEPT: Mutex<Option<(std::time::Instant, usize)>> = Mutex::const_new(None);
/// How long the refresh key shows what it did.
const SWEPT_SHOWN: Duration = Duration::from_secs(5);

fn lane_of(settings: &serde_json::Value) -> Option<Tool> {
	match settings.get("tool").and_then(serde_json::Value::as_str)?.to_ascii_lowercase().as_str() {
		"claude" => Some(Tool::Claude),
		"codex" => Some(Tool::Codex),
		_ => None,
	}
}

/// The AI sessions to show, each with its desktop index, in display order:
/// by desktop, then Claude before Codex, then name.
///
/// A session whose desktop is not known (or no longer exists) is left out:
/// its key could not take the user anywhere. It shows up once a prompt or a
/// window tells where it is.
fn agent_view(snap: &Snapshot) -> Vec<(View, usize)> {
	let sessions = if crate::demo::on() { demo_sessions(snap) } else { super::visible() };
	let mut unplaced = Vec::new();
	let mut list: Vec<_> = sessions
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

const MINUTE: u64 = 60_000;

/// Made-up sessions, one of each state, on the first five desktops, in display
/// order: (tool, name, state, desktop index, time in that state). Shown in
/// demo mode (`crate::demo`) and in the `--selftest` preview.
const SAMPLES: [(Tool, &str, State, usize, u64); 9] = [
	(Tool::Claude, "inbox", State::Done, 0, 25 * MINUTE),
	(Tool::Claude, "web-app", State::Permission, 1, MINUTE / 2),
	(Tool::Codex, "web-app", State::Working, 1, 4 * MINUTE),
	(Tool::Claude, "api", State::Working, 2, 2 * MINUTE),
	(Tool::Codex, "api-tests", State::Reviewing, 2, 0),
	(Tool::Claude, "docs", State::Background, 3, 6 * MINUTE),
	(Tool::Codex, "docs-review", State::Done, 3, 12 * MINUTE),
	(Tool::Claude, "mobile", State::Input, 4, 3 * MINUTE),
	(Tool::Codex, "mobile", State::Idle, 4, 40 * MINUTE),
];

/// The made-up sessions for demo mode, on the desktops there are.
fn demo_sessions(snap: &Snapshot) -> Vec<View> {
	let now = super::now_ms();
	SAMPLES
		.into_iter()
		.enumerate()
		.filter_map(|(i, (tool, label, state, desktop, age))| {
			let id = format!("demo:{i}");
			let desktop = snap.ids.get(desktop)?.clone();
			let detail = Detail {
				folder: Some(label.to_owned()),
				placed_by: "App window",
				background: if state == State::Background { vec!["shell".to_owned(), "subagent".to_owned()] } else { Vec::new() },
				asks: usize::from(state == State::Permission),
				last: now.saturating_sub(age),
			};
			let since = now.saturating_sub(age);
			Some(View { id, tool, label: label.to_owned(), state, stale: false, since, desktop: Some(desktop), detail })
		})
		.collect()
}

/// Sessions last left out of `agent_view` for want of a desktop.
static UNPLACED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

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
	fn attention_toward(&self, next: bool, sessions: &[(View, usize)]) -> bool {
		sessions.iter().zip(&self.page_of).any(|((s, _), page)| {
			s.state.needs_user() && page.is_some_and(|p| if next { p > self.page } else { p < self.page })
		})
	}
}

/// The AI Session keys of every device, laid out for the page it is on.
async fn agent_pages(sessions: &[(View, usize)]) -> HashMap<String, AgentPage> {
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
fn card(s: &View, desktop: usize, current: usize, now: u64) -> AgentCard<'_> {
	AgentCard {
		tool: s.tool,
		name: &s.label,
		state: s.state,
		stale: s.stale,
		desktop,
		elapsed_ms: now.saturating_sub(s.since),
		here: desktop == current,
	}
}

/// Draw every visible AI key.
pub async fn draw(snap: &Snapshot) {
	let now = super::now_ms();
	let sessions = agent_view(snap);
	let pages = agent_pages(&sessions).await;
	let setup = setup_open().await;
	if setup {
		// The summary key's settings take over the session keys.
		let prefs = prefs::get();
		for (slot, option) in setup_keys().await {
			let svg = option.map_or_else(crate::render::empty, |tool| render::jump_option(tool, prefs.jumps_to(tool)));
			send(&slot, svg).await;
		}
	} else {
		let detail = detail_shown(&sessions).await;
		for (slot, pick) in pages.values().flat_map(|p| &p.keys) {
			if detail.as_ref().is_some_and(|(device, _)| *device == slot.device_id) {
				continue; // that device shows the detail screen instead
			}
			let svg = match pick.map(|i| &sessions[i]) {
				Some((s, desktop)) => render::agent(&card(s, *desktop, snap.current, now)),
				None => crate::render::empty(),
			};
			send(slot, svg).await;
		}
		if let Some((device, i)) = &detail {
			draw_detail(device, &sessions[*i], snap, now).await;
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
		let count = |f: &dyn Fn(&View) -> bool| sessions.iter().filter(|(s, _)| f(s)).count();
		let attention = count(&|s| s.state.needs_user());
		let done = count(&|s| s.state == State::Done);
		let working = count(&|s| matches!(s.state, State::Working | State::Background | State::Reviewing) && !s.stale);
		for key in summary_keys {
			let svg = if setup { render::close_button("Close") } else { render::summary(attention, done, working) };
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
/// Counts presses of keys that can be held (the summary and session keys).
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

/// Every visible AI Session key with the checkbox it shows in the settings
/// screen, if any.
async fn setup_keys() -> Vec<(Arc<Instance>, Option<Tool>)> {
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

// ---- a session key's detail screen
//
// Holding an AI Session key turns that device's AI Session keys into a screen
// about its session: what is known about it, and keys to jump to it, take it
// off the deck, or close the screen. Like the settings screen, it is drawn
// over the keys that are there.

/// What the detail screen shows on each AI Session key, in key order (the
/// suggested layout's two rows of five). Keys beyond these stay dark.
#[derive(Clone, Copy, Debug, PartialEq)]
enum DetailKey {
	/// The session's own key, to tell which it is.
	Card,
	Desktop,
	Folder,
	PlacedBy,
	LastEvent,
	Jump,
	Hide,
	Background,
	Asks,
	Close,
}

const DETAIL_KEYS: [DetailKey; 10] = [
	DetailKey::Card,
	DetailKey::Desktop,
	DetailKey::Folder,
	DetailKey::PlacedBy,
	DetailKey::LastEvent,
	DetailKey::Jump,
	DetailKey::Hide,
	DetailKey::Background,
	DetailKey::Asks,
	DetailKey::Close,
];

/// The open detail screen: on which device, about which session (`View::id`).
struct DetailScreen {
	device: String,
	session: String,
	touched: std::time::Instant,
}

static DETAIL: Mutex<Option<DetailScreen>> = Mutex::const_new(None);
/// The AI Session key held down, until its release.
static SLOT_HELD: Mutex<Option<Held>> = Mutex::const_new(None);

#[derive(Clone, PartialEq)]
struct Held {
	key: InstanceId,
	press: u64,
	/// It has turned into a long press: its release is not a press.
	long: bool,
}

/// The open detail screen as (device, session); it closes by itself after
/// `SETUP_IDLE` without a press.
async fn detail_open() -> Option<(String, String)> {
	let mut detail = DETAIL.lock().await;
	if detail.as_ref().is_some_and(|d| d.touched.elapsed() > SETUP_IDLE) {
		*detail = None;
	}
	detail.as_ref().map(|d| (d.device.clone(), d.session.clone()))
}

/// The detail screen to draw: its device and its session's index in
/// `sessions`. A screen whose session has gone closes.
async fn detail_shown(sessions: &[(View, usize)]) -> Option<(String, usize)> {
	let (device, session) = detail_open().await?;
	match sessions.iter().position(|(s, _)| s.id == session) {
		Some(i) => Some((device, i)),
		None => {
			*DETAIL.lock().await = None;
			None
		}
	}
}

/// `device`'s AI Session keys, in order.
async fn device_slots(device: &str) -> Vec<Arc<Instance>> {
	ordered(AGENT).await.into_iter().filter(|s| s.device_id == device).collect()
}

/// Draw the detail screen over `device`'s AI Session keys.
async fn draw_detail(device: &str, (s, desktop): &(View, usize), snap: &Snapshot, now: u64) {
	let slots = device_slots(device).await;
	for (k, slot) in slots.iter().enumerate() {
		let svg = DETAIL_KEYS.get(k).map_or_else(crate::render::empty, |what| detail_key(*what, s, *desktop, snap, now));
		send(slot, svg).await;
	}
}

fn detail_key(what: DetailKey, s: &View, desktop: usize, snap: &Snapshot, now: u64) -> String {
	let d = &s.detail;
	match what {
		DetailKey::Card => render::agent(&card(s, desktop, snap.current, now)),
		DetailKey::Desktop => {
			let name = snap.names.get(desktop).filter(|n| !n.is_empty()).map(String::as_str);
			render::info("Desktop", &(desktop + 1).to_string(), Some(name.unwrap_or("(no name)")))
		}
		DetailKey::Folder => render::info("Folder", d.folder.as_deref().unwrap_or("-"), None),
		DetailKey::PlacedBy => render::info("Placed by", d.placed_by, None),
		DetailKey::LastEvent => {
			let ago = if d.last == 0 { "none".to_owned() } else { render::ago(now.saturating_sub(d.last)) };
			render::info("Last event", &ago, None)
		}
		DetailKey::Background => {
			let kinds = tally(&d.background);
			render::info("Background", kinds.first().map_or("none", String::as_str), kinds.get(1).map(String::as_str))
		}
		DetailKey::Asks => render::info("Approvals", &d.asks.to_string(), Some("waiting")),
		DetailKey::Jump => render::jump_button(desktop),
		DetailKey::Hide => render::hide_button(),
		DetailKey::Close => render::close_button("Close"),
	}
}

/// "shell", "monitor", "monitor" -> "monitor ×2", "shell": most first.
fn tally(kinds: &[String]) -> Vec<String> {
	let mut counts: Vec<(&str, usize)> = Vec::new();
	for kind in kinds {
		match counts.iter_mut().find(|(k, _)| k == kind) {
			Some((_, n)) => *n += 1,
			None => counts.push((kind, 1)),
		}
	}
	counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
	counts.into_iter().map(|(k, n)| if n > 1 { format!("{k} ×{n}") } else { k.to_owned() }).collect()
}

/// A press on the detail screen's key `instance` of `device`, about `session`.
async fn press_detail(instance: &Instance, device: &str, session: &str) {
	let k = device_slots(device).await.iter().position(|s| s.instance_id == instance.instance_id);
	let close = match k.and_then(|k| DETAIL_KEYS.get(k)) {
		Some(DetailKey::Jump) => {
			let sessions = agent_view(&desktops::get().snapshot());
			if let Some((_, desktop)) = sessions.iter().find(|(s, _)| s.id == session) {
				desktops::get().switch(Target::Index(*desktop));
			}
			true
		}
		Some(DetailKey::Hide) => {
			super::hide(session);
			true
		}
		Some(DetailKey::Close) => true,
		_ => false,
	};
	let mut detail = DETAIL.lock().await;
	if close {
		*detail = None;
	} else if let Some(d) = detail.as_mut() {
		d.touched = std::time::Instant::now();
	}
	drop(detail);
	redraw_all().await;
}

/// The index in `sessions` of the session on `device`'s AI Session key `key`.
async fn picked(sessions: &[(View, usize)], device: &str, key: &InstanceId) -> Option<usize> {
	let pages = agent_pages(sessions).await;
	pages.get(device).and_then(|p| p.keys.iter().find(|(slot, _)| slot.instance_id == *key)).and_then(|(_, pick)| *pick)
}

// ---- actions

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
		let lane = lane_of(settings);
		log::info!("agent slot settings changed: {} now for {lane:?}", describe(instance));
		LANES.lock().await.insert(instance.instance_id.clone(), lane);
		redraw_all().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		LANES.lock().await.remove(&instance.instance_id);
		forget(instance).await;
		Ok(())
	}

	/// Held long enough on a session, the key opens its detail screen
	/// (without waiting for the release). Not while a screen is open: then
	/// the keys are that screen's.
	async fn key_down(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		if setup_open().await || detail_open().await.is_some_and(|(device, _)| device == instance.device_id) {
			return Ok(());
		}
		let held = Held { key: instance.instance_id.clone(), press: PRESSES.fetch_add(1, Ordering::Relaxed) + 1, long: false };
		*SLOT_HELD.lock().await = Some(held.clone());
		let device = instance.device_id.clone();
		tokio::spawn(async move {
			tokio::time::sleep(LONG_PRESS).await;
			{
				let mut now_held = SLOT_HELD.lock().await;
				if now_held.as_ref() != Some(&held) {
					return; // released, or another key pressed, in the meantime
				}
				*now_held = Some(Held { long: true, ..held.clone() });
			}
			let key = held.key;
			let sessions = agent_view(&desktops::get().snapshot());
			let Some(i) = picked(&sessions, &device, &key).await else { return };
			log::info!("{:?} {}: key held, detail screen", sessions[i].0.tool, sessions[i].0.label);
			let session = sessions[i].0.id.clone();
			*DETAIL.lock().await = Some(DetailScreen { device, session, touched: std::time::Instant::now() });
			redraw_all().await;
		});
		Ok(())
	}

	/// A short press: go to the desktop the session is on; an empty slot does
	/// nothing. In the settings screen, tick or clear the key's checkbox; in
	/// the detail screen, do what the key there says.
	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let held = SLOT_HELD.lock().await.take().filter(|h| h.key == instance.instance_id);
		if held.as_ref().is_some_and(|h| h.long) {
			return Ok(()); // the end of a long press
		}
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
		if let Some((device, session)) = detail_open().await.filter(|(device, _)| *device == instance.device_id) {
			press_detail(instance, &device, &session).await;
			return Ok(());
		}
		if held.is_none() {
			return Ok(()); // pressed while a screen was open, released after it closed
		}
		let sessions = agent_view(&desktops::get().snapshot());
		if let Some(i) = picked(&sessions, &instance.device_id, &instance.instance_id).await {
			desktops::get().switch(Target::Index(sessions[i].1));
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
		let sessions = agent_view(&desktops::get().snapshot());
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
		desktops::get().refresh();
		let removed = super::sweep().await;
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
		// leaving the profile closes the settings and detail screens
		*SETUP.lock().await = None;
		*DETAIL.lock().await = None;
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
			*DETAIL.lock().await = None;
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
		let sessions = agent_view(&desktops::get().snapshot());
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
		desktops::get().switch(Target::Index(desktop));
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

// ---- preview for --selftest

/// The suggested 3x5 layout (`layout/AI.json`) with the made-up sessions,
/// plus a stalled one on a sixth desktop to fill a second page; then the
/// summary key's settings screen. Each is a title and 15 key images.
pub fn preview() -> Vec<(String, Vec<String>)> {
	// As if desktop 3 were showing.
	let card = |(tool, name, state, desktop, age): (Tool, &'static str, State, usize, u64)| AgentCard {
		tool,
		name,
		state,
		stale: false,
		desktop,
		elapsed_ms: age,
		here: desktop == 2,
	};
	let mut samples: Vec<AgentCard> = SAMPLES.into_iter().map(card).collect();
	samples.push(AgentCard { stale: true, ..card((Tool::Claude, "infra", State::Working, 5, 20 * MINUTE)) });
	// Claude row, Codex row, then [→Default][refresh][◀][summary][▶].
	let lanes: Vec<Option<Tool>> = [Some(Tool::Claude); 5].into_iter().chain([Some(Tool::Codex); 5]).collect();
	let placed: Vec<(Tool, usize)> = samples.iter().map(|s| (s.tool, s.desktop)).collect();
	let Pages { keys, page_of } = paginate(&lanes, &placed);
	let pages = keys.len();
	let mut out = Vec::new();
	for (page, picks) in keys.iter().enumerate() {
		let mut ai: Vec<String> =
			picks.iter().map(|pick| pick.map_or_else(crate::render::empty, |i| render::agent(&samples[i]))).collect();
		let attention = |next: bool| {
			samples.iter().zip(&page_of).any(|(s, p)| {
				s.state.needs_user() && p.is_some_and(|p| if next { p > page } else { p < page })
			})
		};
		ai.push(crate::render::empty()); // the Starter Pack switch to Default
		ai.push(render::refresh((page > 0).then_some(2)));
		ai.push(render::page_arrow(false, page.checked_sub(1).map(|p| (p, pages)), attention(false)));
		ai.push(render::summary(2, 2, 2));
		ai.push(render::page_arrow(true, (page + 1 < pages).then_some((page + 1, pages)), attention(true)));
		out.push((format!("AI page {}/{pages} (sample data)", page + 1), ai));
	}

	// The summary key's settings screen, with Codex cleared.
	let mut setup: Vec<String> = (0..10)
		.map(|k| match k {
			0 => render::jump_option(Tool::Claude, true),
			5 => render::jump_option(Tool::Codex, false),
			_ => crate::render::empty(),
		})
		.collect();
	setup.extend([
		crate::render::empty(),
		render::refresh(None),
		render::page_arrow(false, None, false),
		render::close_button("Close"),
		render::page_arrow(true, None, false),
	]);
	out.push(("AI: summary key held (settings)".to_owned(), setup));

	// The detail screen of the Background sample.
	let snap = Snapshot {
		names: ["", "web-app", "api", "docs", "mobile", "design"].map(str::to_owned).to_vec(),
		ids: (0..6).map(|i| format!("D{i}")).collect(),
		current: 2,
	};
	let now = super::now_ms();
	let sessions = demo_sessions(&snap);
	let (s, desktop) = sessions
		.iter()
		.find(|s| s.state == State::Background)
		.and_then(|s| Some((s, snap.ids.iter().position(|d| Some(d) == s.desktop.as_ref())?)))
		.expect("a Background sample");
	let mut detail: Vec<String> = DETAIL_KEYS.iter().map(|what| detail_key(*what, s, desktop, &snap, now)).collect();
	detail.extend([
		crate::render::empty(),
		render::refresh(None),
		render::page_arrow(false, None, false),
		render::summary(2, 2, 4),
		render::page_arrow(true, None, false),
	]);
	out.push(("AI: session key held (detail)".to_owned(), detail));
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn background_kinds_are_counted_most_first() {
		let kinds: Vec<String> = ["shell", "monitor", "subagent", "monitor"].map(str::to_owned).to_vec();
		assert_eq!(tally(&kinds), vec!["monitor ×2", "shell", "subagent"]);
		assert!(tally(&[]).is_empty());
	}

	#[test]
	fn the_detail_screen_has_its_three_actions_on_the_second_row() {
		let at = |what| DETAIL_KEYS.iter().position(|k| *k == what);
		assert_eq!((at(DetailKey::Card), at(DetailKey::Jump), at(DetailKey::Hide), at(DetailKey::Close)), (Some(0), Some(5), Some(6), Some(9)));
	}
}
