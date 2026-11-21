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
//!   Press to go to that session's desktop.
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

static VD: OnceLock<vd::Handle> = OnceLock::new();

fn vd() -> &'static vd::Handle {
	VD.get().expect("virtual desktop worker not started")
}

/// Last image sent to each key, so unchanged keys are not redrawn.
static SENT: LazyLock<Mutex<HashMap<InstanceId, String>>> = LazyLock::new(Default::default);
/// One redraw at a time.
static REDRAW: Mutex<()> = Mutex::const_new(());

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

fn neighbour(snap: &vd::Snapshot, next: bool) -> Option<(usize, &str)> {
	let index = if next { snap.current.checked_add(1)? } else { snap.current.checked_sub(1)? };
	snap.names.get(index).map(|n| (index, n.as_str()))
}

/// Visible AI sessions with their desktop index, in display order:
/// by desktop, then Claude before Codex, then name.
fn agent_view(snap: &vd::Snapshot) -> Vec<(agents::Session, Option<usize>)> {
	let mut list: Vec<_> = agents::visible()
		.into_iter()
		.map(|s| {
			let index = s.desktop.as_ref().and_then(|g| snap.ids.iter().position(|x| x == g));
			(s, index)
		})
		.collect();
	list.sort_by_cached_key(|(s, i)| (i.unwrap_or(usize::MAX), s.tool, s.label()));
	list
}

fn card<'a>(s: &'a agents::Session, label: &'a str, desktop: Option<usize>, now: u64) -> render::AgentCard<'a> {
	render::AgentCard {
		tool: s.tool,
		name: label,
		state: s.state,
		stale: s.is_stale(now),
		desktop,
		elapsed_ms: now.saturating_sub(s.since),
	}
}

async fn redraw_all() {
	let _guard = REDRAW.lock().await;
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
	let mut seen_per_device: HashMap<String, usize> = HashMap::new();
	for slot in ordered(AGENT).await {
		let counter = seen_per_device.entry(slot.device_id.clone()).or_insert(0);
		let index = *counter;
		*counter += 1;
		let svg = match sessions.get(index) {
			Some((s, desktop)) => {
				let label = s.label();
				render::agent(&card(s, &label, *desktop, now))
			}
			None => render::empty(),
		};
		send(&slot, svg).await;
	}
	let summary_keys = visible_instances(AGENTS).await;
	if !summary_keys.is_empty() {
		let count = |f: &dyn Fn(&agents::Session) -> bool| sessions.iter().filter(|(s, _)| f(s)).count();
		let attention = count(&|s| matches!(s.state, agents::State::Permission | agents::State::Input | agents::State::Error));
		let done = count(&|s| s.state == agents::State::Done);
		let working = count(&|s| s.state == agents::State::Working && !s.is_stale(now));
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
		redraw_all().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		redraw_all().await;
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
		redraw_all().await;
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
		redraw_all().await;
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

	async fn will_appear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		log::info!("agent slot appeared: {}", describe(instance));
		redraw_all().await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		forget(instance).await;
		Ok(())
	}

	/// Go to the desktop the session is on. A session whose desktop is not
	/// known yet (shown as "?") gets an alert; an empty slot does nothing.
	async fn key_up(&self, instance: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		let sessions = agent_view(&vd().snapshot());
		let slots = ordered(AGENT).await;
		match slot_index(&slots, instance).and_then(|i| sessions.get(i)) {
			Some((_, Some(desktop))) => vd().switch(vd::Target::Index(*desktop)),
			Some((s, None)) => {
				log::info!("{:?} {}: desktop not known yet", s.tool, s.label());
				instance.show_alert().await?
			}
			None => {}
		}
		Ok(())
	}
}

struct AgentsSummaryAction;
#[async_trait]
impl Action for AgentsSummaryAction {
	const UUID: ActionUuid = AGENTS;
	type Settings = serde_json::Value;

	async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
		redraw_all().await;
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
	let file = to_file
		.then(|| std::env::current_exe().ok().map(|p| p.with_file_name("vdesk.log")))
		.flatten()
		.and_then(|p| std::fs::File::create(p).ok());
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

	// AI profile with sample sessions, one of each state.
	use agents::{State, Tool};
	let samples: [(Tool, &str, State, bool, Option<usize>, u64); 7] = [
		(Tool::Claude, "GMK01", State::Working, false, Some(2), 3 * 60_000),
		(Tool::Codex, "MatterMost", State::Done, false, Some(2), 12 * 60_000),
		(Tool::Claude, "GMK02", State::Permission, false, Some(3), 40_000),
		(Tool::Claude, "Fujitu01", State::Done, false, Some(4), 2 * 3_600_000),
		(Tool::Codex, "見積もり整理レセコン", State::Input, false, Some(4), 5 * 60_000),
		(Tool::Claude, "Epson01", State::Working, true, Some(5), 20 * 60_000),
		(Tool::Codex, "DoveBar", State::Idle, false, None, 0),
	];
	let mut ai: Vec<String> = samples
		.iter()
		.map(|(tool, name, state, stale, desktop, elapsed)| {
			render::agent(&render::AgentCard {
				tool: *tool,
				name,
				state: *state,
				stale: *stale,
				desktop: *desktop,
				elapsed_ms: *elapsed,
			})
		})
		.collect();
	while ai.len() < 13 {
		ai.push(render::empty());
	}
	ai.push(render::agents_summary(2, 2, 1));
	ai.push(render::empty());

	let html = format!(
		r#"<!doctype html><meta charset="utf-8"><title>vdesk preview</title><style>body{{background:#111;color:#ccc;font-family:sans-serif;margin:24px}}div{{display:grid;grid-template-columns:repeat(5,96px);gap:12px;margin-bottom:24px}}img{{width:96px;height:96px;border-radius:12px}}h2{{font-size:14px}}</style>{}{}"#,
		preview_grid("Default", &desk),
		preview_grid("AI (sample data)", &ai)
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
