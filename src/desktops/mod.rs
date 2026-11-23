//! The desktop part: Windows 11 virtual desktops, and the keys that show and
//! switch them (`keys`). It knows nothing of AI agents; the agent part uses
//! only `get()` to see the desktops and switch between them.
//!
//! Every winvd (undocumented Windows COM) call runs on one dedicated thread.
//! That thread publishes the current list of desktops through a `watch`
//! channel and redraws are driven from there. It wakes up on winvd's change
//! notifications and also re-reads every few seconds, so the keys stay right
//! even if a notification is missed or explorer.exe restarts.

pub mod keys;
mod render;
mod sendkeys;

use std::sync::OnceLock;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use tokio::sync::watch;

static HANDLE: OnceLock<Handle> = OnceLock::new();

/// Start the worker thread; once, before anything uses `get()`.
pub fn init() {
	let _ = HANDLE.set(start());
}

/// The desktops: what there is, and switching between them.
pub fn get() -> &'static Handle {
	HANDLE.get().expect("desktops::init was not called")
}

/// How often the desktop list is re-read when nothing else happens.
const POLL: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
	/// Desktop names in Windows order. An empty string means "never renamed".
	pub names: Vec<String>,
	/// Desktop GUIDs in the same order (stable across renames and reordering).
	pub ids: Vec<String>,
	/// Index of the desktop that is showing now.
	pub current: usize,
}

impl Snapshot {
	pub fn count(&self) -> usize {
		self.names.len()
	}
}

#[derive(Clone, Copy, Debug)]
pub enum Target {
	Index(usize),
	Prev,
	Next,
}

#[derive(Clone)]
enum Msg {
	/// winvd reported a change worth redrawing for
	Changed,
	/// winvd reported something we do not show (a window moved, wallpaper)
	Ignored,
	Refresh,
	Switch(Target),
}

impl From<winvd::DesktopEvent> for Msg {
	fn from(event: winvd::DesktopEvent) -> Self {
		use winvd::DesktopEvent::*;
		match event {
			DesktopCreated(_) | DesktopDestroyed { .. } | DesktopChanged { .. } | DesktopNameChanged(..) | DesktopMoved { .. } => {
				Msg::Changed
			}
			DesktopWallpaperChanged(..) | WindowChanged(_) => Msg::Ignored,
		}
	}
}

pub struct Handle {
	tx: mpsc::Sender<Msg>,
	state: watch::Receiver<Snapshot>,
}

impl Handle {
	pub fn switch(&self, target: Target) {
		let _ = self.tx.send(Msg::Switch(target));
	}

	pub fn refresh(&self) {
		let _ = self.tx.send(Msg::Refresh);
	}

	pub fn snapshot(&self) -> Snapshot {
		self.state.borrow().clone()
	}

	pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
		self.state.clone()
	}
}

/// Start the worker thread. The first snapshot is read before this returns.
fn start() -> Handle {
	let (tx, rx) = mpsc::channel::<Msg>();
	let initial = read().unwrap_or_else(|e| {
		log::warn!("could not read desktops at startup: {e:?}");
		Snapshot::default()
	});
	let (state_tx, state_rx) = watch::channel(initial);
	let listener_tx = tx.clone();
	std::thread::Builder::new()
		.name("agentj-worker".into())
		.spawn(move || worker(rx, listener_tx, state_tx))
		.expect("failed to start the virtual desktop thread");
	Handle { tx, state: state_rx }
}

fn worker(rx: mpsc::Receiver<Msg>, listener_tx: mpsc::Sender<Msg>, state_tx: watch::Sender<Snapshot>) {
	// Keep the listener alive for as long as the worker runs.
	let mut listener = start_listener(&listener_tx);
	loop {
		match rx.recv_timeout(POLL) {
			Ok(Msg::Ignored) => continue,
			Ok(Msg::Changed | Msg::Refresh) => {}
			Ok(Msg::Switch(target)) => {
				let snap = state_tx.borrow().clone();
				switch(target, &snap);
			}
			Err(RecvTimeoutError::Timeout) => {
				if listener.is_none() {
					listener = start_listener(&listener_tx);
				}
			}
			Err(RecvTimeoutError::Disconnected) => break,
		}
		publish(&state_tx);
	}
}

fn start_listener(tx: &mpsc::Sender<Msg>) -> Option<winvd::DesktopEventThread> {
	match winvd::listen_desktop_events(tx.clone()) {
		Ok(thread) => Some(thread),
		Err(e) => {
			log::warn!("desktop change notifications unavailable, relying on polling: {e:?}");
			None
		}
	}
}

/// Read the desktop list from Windows.
pub fn read() -> winvd::Result<Snapshot> {
	let desktops = winvd::get_desktops()?;
	let names = desktops.iter().map(|d| d.get_name().unwrap_or_default()).collect();
	let ids = desktops.iter().map(|d| d.get_id().map(|g| format!("{g:?}")).unwrap_or_default()).collect();
	let current = winvd::get_current_desktop()?.get_index()? as usize;
	Ok(Snapshot { names, ids, current })
}

fn publish(state_tx: &watch::Sender<Snapshot>) {
	match read() {
		Ok(snap) => {
			state_tx.send_if_modified(|old| {
				if *old == snap {
					return false;
				}
				log::info!("desktops: {} (current {}) {:?}", snap.count(), snap.current + 1, snap.names);
				*old = snap;
				true
			});
		}
		Err(e) => log::warn!("could not read desktops: {e:?}"),
	}
}

fn switch(target: Target, snap: &Snapshot) {
	let count = snap.count();
	let index = match target {
		Target::Index(i) => i,
		Target::Prev => match snap.current.checked_sub(1) {
			Some(i) => i,
			None => return,
		},
		Target::Next => snap.current + 1,
	};
	if index >= count || index == snap.current {
		return;
	}
	match winvd::switch_desktop(index as u32) {
		Ok(()) => log::info!("switched to desktop {}", index + 1),
		Err(e) => {
			// The COM interface changes between Windows builds. The keyboard
			// shortcut always works, it just has to step one desktop at a time.
			log::warn!("direct switch failed ({e:?}), stepping with Win+Ctrl+arrow");
			sendkeys::step(index as i64 - snap.current as i64);
		}
	}
}
