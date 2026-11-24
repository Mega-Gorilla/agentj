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
use super::{State, Tool, View, prefs};
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
		.filter_map(|(tool, label, state, desktop, age)| {
			let desktop = snap.ids.get(desktop)?.clone();
			Some(View { tool, label: label.to_owned(), state, stale: false, since: now.saturating_sub(age), desktop: Some(desktop) })
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
		for (slot, pick) in pages.values().flat_map(|p| &p.keys) {
			let svg = match pick.map(|i| &sessions[i]) {
				Some((s, desktop)) => render::agent(&card(s, *desktop, snap.current, now)),
				None => crate::render::empty(),
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
		let count = |f: &dyn Fn(&View) -> bool| sessions.iter().filter(|(s, _)| f(s)).count();
		let attention = count(&|s| s.state.needs_user());
		let done = count(&|s| s.state == State::Done);
		let working = count(&|s| matches!(s.state, State::Working | State::Background | State::Reviewing) && !s.stale);
		for key in summary_keys {
			let svg = if setup { render::settings_close() } else { render::summary(attention, done, working) };
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
		let sessions = agent_view(&desktops::get().snapshot());
		let pages = agent_pages(&sessions).await;
		let pick = pages
			.get(&instance.device_id)
			.and_then(|p| p.keys.iter().find(|(slot, _)| slot.instance_id == instance.instance_id))
			.and_then(|(_, pick)| *pick);
		if let Some(i) = pick {
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
		render::settings_close(),
		render::page_arrow(true, None, false),
	]);
	out.push(("AI: summary key held (settings)".to_owned(), setup));
	out
}
