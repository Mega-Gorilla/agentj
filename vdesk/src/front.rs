//! Which windows have been in front lately.
//!
//! A prompt's hook arrives a moment after the prompt was sent (about 2 s for
//! Codex), by which time the user may have clicked another window. Asking
//! "what is in front now" then misses the window the prompt was typed into,
//! so a short history is kept instead.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use crate::winutil::{self, WindowInfo};

/// How often the window in front is looked at.
const POLL: Duration = Duration::from_millis(250);
/// How much history is kept.
const KEEP_MS: u64 = 30_000;

/// (when it came to the front, the window), oldest first. Each one stayed in
/// front until the next one's time.
static HISTORY: Mutex<VecDeque<(u64, WindowInfo)>> = Mutex::new(VecDeque::new());

/// Start watching. Costs one cheap call per poll; the window's program and
/// desktop are looked up only when the window in front changes.
pub fn start() {
	std::thread::spawn(|| {
		let mut last = None;
		loop {
			let key = winutil::foreground_key();
			if key != last {
				if let Some(window) = winutil::foreground() {
					let now = crate::agents::now_ms();
					let mut history = HISTORY.lock().unwrap();
					history.push_back((now, window));
					trim(&mut history, now.saturating_sub(KEEP_MS));
				}
				last = key;
			}
			std::thread::sleep(POLL);
		}
	});
}

/// Drop what stopped being in front before `oldest`.
fn trim(history: &mut VecDeque<(u64, WindowInfo)>, oldest: u64) {
	// The first entry was in front until the second one's time.
	while history.len() > 1 && history[1].0 <= oldest {
		history.pop_front();
	}
}

/// The windows that were in front at any time from `since` on: first the one
/// in front at `sent` (the best guess for where something sent then was
/// typed), then the others, newest first.
pub fn around(sent: u64, since: u64) -> Vec<WindowInfo> {
	in_front_around(HISTORY.lock().unwrap().make_contiguous(), sent, since)
}

fn in_front_around(history: &[(u64, WindowInfo)], sent: u64, since: u64) -> Vec<WindowInfo> {
	// The one in front at a time is the last that came to the front by then.
	let at = |time: u64| history.iter().rposition(|(at, _)| *at <= time).unwrap_or(0);
	let (first, sent) = (at(since.min(sent)), at(sent));
	let mut order: Vec<usize> = (first..history.len()).rev().collect();
	if let Some(position) = order.iter().position(|&i| i == sent) {
		let i = order.remove(position);
		order.insert(0, i);
	}
	order.into_iter().map(|i| history[i].1.clone()).collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	fn window(title: &str) -> WindowInfo {
		WindowInfo { process: "p".to_owned(), title: title.to_owned(), desktop: None }
	}

	fn titles(history: &[(u64, WindowInfo)], sent: u64, since: u64) -> Vec<String> {
		in_front_around(history, sent, since).into_iter().map(|w| w.title).collect()
	}

	#[test]
	fn the_window_in_front_when_sent_comes_first_then_the_newest() {
		let history = [(100, window("a")), (200, window("b")), (300, window("c")), (400, window("d"))];
		// sent while "b" was in front; "a" had left before the period began
		assert_eq!(titles(&history, 250, 210), ["b", "d", "c"]);
		// sent just now: plain newest first, with the one already in front at the start
		assert_eq!(titles(&history, 999, 250), ["d", "c", "b"]);
		assert_eq!(titles(&history, 999, 999), ["d"]);
		// a period starting after the send still reaches back to the send
		assert_eq!(titles(&history, 150, 350), ["a", "d", "c", "b"]);
		assert!(titles(&[], 0, 0).is_empty());
	}

	#[test]
	fn trimming_keeps_what_was_still_in_front() {
		let mut history: VecDeque<_> = [(100, window("a")), (200, window("b")), (300, window("c"))].into();
		trim(&mut history, 250); // "a" left the front at 200; "b" stayed until 300
		assert_eq!(history.iter().map(|(at, _)| *at).collect::<Vec<_>>(), [200, 300]);
		trim(&mut history, 10_000);
		assert_eq!(history.len(), 1); // the one in front now always stays
	}
}
