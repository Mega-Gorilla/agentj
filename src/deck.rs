//! What every key needs, whichever part it belongs to: sending images without
//! repeating them, finding keys in reading order, and redrawing.
//!
//! The two parts draw their own keys (`desktops::keys`, `agents::keys`);
//! this module only calls them, and is the one place that knows both.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use openaction::*;
use tokio::sync::Mutex;

/// Last image sent to each key, so unchanged keys are not redrawn.
static SENT: LazyLock<Mutex<HashMap<InstanceId, String>>> = LazyLock::new(Default::default);
/// One redraw at a time.
static REDRAW: Mutex<()> = Mutex::const_new(());

/// Visible keys of one action in reading order, grouped by device.
pub async fn ordered(action: ActionUuid) -> Vec<Arc<Instance>> {
	let mut keys: Vec<_> = visible_instances(action).await.into_iter().filter(|i| i.coordinates.is_some()).collect();
	keys.sort_by_key(|i| {
		let c = i.coordinates.expect("filtered above");
		(i.device_id.clone(), c.row, c.column)
	});
	keys
}

/// Send `svg` to the key, unless it already shows it.
pub async fn send(instance: &Instance, svg: String) {
	let url = crate::render::data_url(&svg);
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

/// The key went away: whatever it showed is no longer known.
pub async fn forget(instance: &Instance) {
	SENT.lock().await.remove(&instance.instance_id);
}

/// A key and where it is, for the log.
pub fn describe(instance: &Instance) -> String {
	match instance.coordinates {
		Some(c) => format!("{} at row {} col {}", instance.instance_id, c.row, c.column),
		None => format!("{} (no position)", instance.instance_id),
	}
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
pub async fn appeared() {
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

pub async fn redraw_all() {
	redraw(false).await;
}

/// Draw every visible key. `resend` sends images even where nothing changed.
pub async fn redraw(resend: bool) {
	let _guard = REDRAW.lock().await;
	if resend {
		SENT.lock().await.clear();
	}
	let snap = crate::desktops::get().snapshot();
	crate::desktops::keys::draw(&snap).await;
	crate::agents::keys::draw(&snap).await;
}
