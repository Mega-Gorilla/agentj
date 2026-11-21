//! OpenDeck plugin that switches Windows 11 virtual desktops.
//!
//! Actions:
//! - Desktop (slot): the slot keys on a device are numbered in reading order
//!   (row, then column). Slot N switches to desktop N. Slots beyond the number
//!   of desktops go dark, so the keys follow desktops being added or removed.
//! - Desktop Info: "current / total" and the current name. Press to re-read.
//! - Previous / Next desktop.
//!
//! `vdesk.exe --selftest` prints what it reads from Windows and writes
//! preview.html, without switching anything.

mod keys;
mod render;
mod vd;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};

use openaction::*;
use tokio::sync::Mutex;

const SLOT: ActionUuid = "local.virtualdesktop.slot";
const INFO: ActionUuid = "local.virtualdesktop.info";
const PREV: ActionUuid = "local.virtualdesktop.prev";
const NEXT: ActionUuid = "local.virtualdesktop.next";

static VD: OnceLock<vd::Handle> = OnceLock::new();

fn vd() -> &'static vd::Handle {
	VD.get().expect("virtual desktop worker not started")
}

/// Last image sent to each key, so unchanged keys are not redrawn.
static SENT: LazyLock<Mutex<HashMap<InstanceId, String>>> = LazyLock::new(Default::default);
/// One redraw at a time.
static REDRAW: Mutex<()> = Mutex::const_new(());

/// Visible slot keys in reading order, grouped by device.
async fn ordered_slots() -> Vec<Arc<Instance>> {
	let mut slots: Vec<_> = visible_instances(SLOT).await.into_iter().filter(|i| i.coordinates.is_some()).collect();
	slots.sort_by_key(|i| {
		let c = i.coordinates.expect("filtered above");
		(i.device_id.clone(), c.row, c.column)
	});
	slots
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

async fn redraw_all() {
	let _guard = REDRAW.lock().await;
	let snap = vd().snapshot();

	let mut seen_per_device: HashMap<String, usize> = HashMap::new();
	for slot in ordered_slots().await {
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
		let slots = ordered_slots().await;
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

/// Read-only check: what winvd sees, a no-op switch to the current desktop,
/// and a preview of the key layout.
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

	// 3x5 preview: 10 slots, then prev / info / next.
	let current = snap.names.get(snap.current).map(|n| (snap.current, n.as_str()));
	let mut cells: Vec<String> = (0..10)
		.map(|i| match snap.names.get(i) {
			Some(n) => render::slot(i, n, i == snap.current),
			None => render::empty(),
		})
		.collect();
	cells.push(render::arrow(false, neighbour(&snap, false)));
	cells.push(render::info(current, snap.count()));
	cells.push(render::arrow(true, neighbour(&snap, true)));
	cells.push(render::empty());
	cells.push(render::empty());
	let imgs: String = cells.iter().map(|s| format!(r#"<img src="{}">"#, render::data_url(s))).collect();
	let html = format!(
		r#"<!doctype html><meta charset="utf-8"><title>vdesk preview</title><style>body{{background:#111;margin:24px}}div{{display:grid;grid-template-columns:repeat(5,96px);gap:12px}}img{{width:96px;height:96px;border-radius:12px}}</style><div>{imgs}</div>"#
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

	tokio::spawn(async {
		let mut changes = vd().subscribe();
		while changes.changed().await.is_ok() {
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
