//! The desktop keys:
//! - Desktop (slot): slot keys on a device are numbered in reading order
//!   (row, then column). Slot N switches to desktop N. Slots beyond the number
//!   of desktops go dark, so the keys follow desktops being added or removed.
//! - Desktop Info: "current / total" and the current name. Press to re-read.
//! - Previous / Next desktop.

use std::collections::HashMap;
use std::sync::Arc;

use openaction::*;

use super::{Snapshot, Target, get, render};
use crate::deck::{appeared, describe, forget, ordered, send};

const SLOT: ActionUuid = "io.github.mega-gorilla.agentj.slot";
const INFO: ActionUuid = "io.github.mega-gorilla.agentj.info";
const PREV: ActionUuid = "io.github.mega-gorilla.agentj.prev";
const NEXT: ActionUuid = "io.github.mega-gorilla.agentj.next";

pub async fn register() {
	register_action(SlotAction).await;
	register_action(InfoAction).await;
	register_action(StepAction::<false>).await;
	register_action(StepAction::<true>).await;
}

fn neighbour(snap: &Snapshot, next: bool) -> Option<(usize, &str)> {
	let index = if next { snap.current.checked_add(1)? } else { snap.current.checked_sub(1)? };
	snap.names.get(index).map(|n| (index, n.as_str()))
}

fn slot_index(slots: &[Arc<Instance>], instance: &Instance) -> Option<usize> {
	slots
		.iter()
		.filter(|s| s.device_id == instance.device_id)
		.position(|s| s.instance_id == instance.instance_id)
}

/// Draw every visible desktop key.
pub async fn draw(snap: &Snapshot) {
	let mut seen_per_device: HashMap<String, usize> = HashMap::new();
	for slot in ordered(SLOT).await {
		let counter = seen_per_device.entry(slot.device_id.clone()).or_insert(0);
		let index = *counter;
		*counter += 1;
		let svg = match snap.names.get(index) {
			Some(name) => render::slot(index, name, index == snap.current),
			None => crate::render::empty(),
		};
		send(&slot, svg).await;
	}

	let current = snap.names.get(snap.current).map(|n| (snap.current, n.as_str()));
	for key in visible_instances(INFO).await {
		send(&key, render::info(current, snap.count())).await;
	}
	for key in visible_instances(PREV).await {
		send(&key, render::arrow(false, neighbour(snap, false))).await;
	}
	for key in visible_instances(NEXT).await {
		send(&key, render::arrow(true, neighbour(snap, true))).await;
	}
}

/// The suggested 3x5 layout (`layout/Default.json`) as it would look now, for
/// `--selftest`: 10 slots, then prev / info / next and two keys that are not
/// ours.
pub fn preview(snap: &Snapshot) -> Vec<String> {
	let current = snap.names.get(snap.current).map(|n| (snap.current, n.as_str()));
	let mut keys: Vec<String> = (0..10)
		.map(|i| match snap.names.get(i) {
			Some(n) => render::slot(i, n, i == snap.current),
			None => crate::render::empty(),
		})
		.collect();
	keys.push(render::arrow(false, neighbour(snap, false)));
	keys.push(render::info(current, snap.count()));
	keys.push(render::arrow(true, neighbour(snap, true)));
	keys.push(crate::render::empty());
	keys.push(crate::render::empty());
	keys
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
			Some(i) if i < get().snapshot().count() => get().switch(Target::Index(i)),
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
		get().refresh();
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
		get().switch(if NEXT_DIR { Target::Next } else { Target::Prev });
		Ok(())
	}
}
