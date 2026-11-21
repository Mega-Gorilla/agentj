//! Top-level windows: which app owns them and which virtual desktop they are on.

use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM};
use windows::Win32::System::Threading::{
	OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
	EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW,
	GetWindowThreadProcessId, IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::PWSTR;

#[derive(Clone, Debug)]
pub struct WindowInfo {
	/// Executable name without extension, e.g. "claude", "ChatGPT"
	pub process: String,
	pub title: String,
	/// Virtual desktop GUID, formatted like `vd::Snapshot::ids`
	pub desktop: Option<String>,
}

fn process_name(hwnd: HWND) -> String {
	let mut pid = 0u32;
	unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
	if pid == 0 {
		return String::new();
	}
	unsafe {
		let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
			return String::new();
		};
		let mut buf = [0u16; 1024];
		let mut len = buf.len() as u32;
		let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
		let _ = CloseHandle(handle);
		if !ok {
			return String::new();
		}
		let path = String::from_utf16_lossy(&buf[..len as usize]);
		std::path::Path::new(&path)
			.file_stem()
			.map(|s| s.to_string_lossy().into_owned())
			.unwrap_or_default()
	}
}

fn title(hwnd: HWND) -> String {
	let mut buf = [0u16; 512];
	let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
	String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn desktop_of(hwnd: HWND) -> Option<String> {
	let desktop = match winvd::get_desktop_by_window(hwnd) {
		Ok(d) => d,
		Err(e) => {
			log::debug!("no desktop for window: {e:?}");
			return None;
		}
	};
	let id = desktop.get_id().ok()?;
	Some(format!("{id:?}"))
}

fn info(hwnd: HWND) -> WindowInfo {
	WindowInfo { process: process_name(hwnd), title: title(hwnd), desktop: desktop_of(hwnd) }
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
	let list = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
	list.push(hwnd);
	BOOL(1)
}

/// Visible, unowned, non-tool top-level windows of the given programs.
pub fn app_windows(programs: &[&str]) -> Vec<WindowInfo> {
	let mut all: Vec<HWND> = Vec::new();
	unsafe {
		let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut Vec<HWND> as isize));
	}
	all.into_iter()
		.filter(|&h| unsafe { IsWindowVisible(h) }.as_bool())
		.filter(|&h| unsafe { GetWindow(h, GW_OWNER) }.map(|owner| owner.0.is_null()).unwrap_or(true))
		.filter(|&h| (unsafe { GetWindowLongW(h, GWL_EXSTYLE) } as u32) & WS_EX_TOOLWINDOW.0 == 0)
		.filter(|&h| {
			let name = process_name(h);
			programs.iter().any(|p| p.eq_ignore_ascii_case(&name))
		})
		.map(info)
		.filter(|w| !w.title.is_empty())
		.collect()
}

/// The window the user is typing into right now.
pub fn foreground() -> Option<WindowInfo> {
	let hwnd = unsafe { GetForegroundWindow() };
	if hwnd.0.is_null() {
		return None;
	}
	Some(info(hwnd))
}
