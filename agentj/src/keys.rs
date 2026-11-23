//! Fallback desktop switching with the built-in Win+Ctrl+Left/Right shortcut.

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
	INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput,
	VIRTUAL_KEY, VK_LCONTROL, VK_LEFT, VK_LWIN, VK_RIGHT,
};

fn key(vk: VIRTUAL_KEY, up: bool, extended: bool) -> INPUT {
	let mut flags = KEYBD_EVENT_FLAGS(0);
	if up {
		flags |= KEYEVENTF_KEYUP;
	}
	if extended {
		flags |= KEYEVENTF_EXTENDEDKEY;
	}
	INPUT {
		r#type: INPUT_KEYBOARD,
		Anonymous: INPUT_0 {
			ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
		},
	}
}

/// Move `delta` desktops to the right (positive) or left (negative).
pub fn step(delta: i64) {
	let arrow = if delta > 0 { VK_RIGHT } else { VK_LEFT };
	let combo = [
		key(VK_LWIN, false, true),
		key(VK_LCONTROL, false, false),
		key(arrow, false, true),
		key(arrow, true, true),
		key(VK_LCONTROL, true, false),
		key(VK_LWIN, true, true),
	];
	for _ in 0..delta.unsigned_abs() {
		unsafe {
			SendInput(&combo, std::mem::size_of::<INPUT>() as i32);
		}
		// Give the switch animation time to start, or Windows drops presses.
		std::thread::sleep(Duration::from_millis(250));
	}
}
