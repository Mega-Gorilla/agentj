//! Top-level windows: which app owns them, which virtual desktop they are on,
//! and which Codex thread a ChatGPT window shows.

use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{
	OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{
	CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId,
};
use windows::Win32::UI::WindowsAndMessaging::{
	EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow, GetWindowLongW, GetWindowTextW,
	GetWindowThreadProcessId, IsWindowVisible, WS_EX_TOOLWINDOW,
};
use windows::core::{PWSTR, VARIANT};

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

/// Visible, unowned, non-tool top-level windows of the given programs, on
/// every virtual desktop.
fn top_windows(programs: &[&str]) -> Vec<HWND> {
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
		.collect()
}

/// The titled top-level windows of the given programs.
pub fn app_windows(programs: &[&str]) -> Vec<WindowInfo> {
	top_windows(programs).into_iter().map(info).filter(|w| !w.title.is_empty()).collect()
}

/// The Codex thread each ChatGPT window shows, as `title`, with the window's
/// desktop. Every ChatGPT window is titled just "ChatGPT", but through UI
/// Automation the conversation it shows is a Document element named after
/// the thread. A window that shows no single conversation is left out.
///
/// Asking makes the app build its accessibility tree, which costs it some
/// speed from then on, so this is only done on request (the refresh key).
pub fn chatgpt_threads() -> Vec<WindowInfo> {
	let windows = top_windows(&["ChatGPT"]);
	if windows.is_empty() {
		return Vec::new();
	}
	// Each thread that asks needs COM; asking again on one is harmless.
	let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
	let found = || -> windows::core::Result<Vec<WindowInfo>> {
		let uia: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
		let document = unsafe { uia.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(UIA_DocumentControlTypeId.0)) }?;
		let shown = |hwnd: HWND| -> windows::core::Result<Vec<String>> {
			let documents = unsafe { uia.ElementFromHandle(hwnd)?.FindAll(TreeScope_Descendants, &document) }?;
			let mut names = Vec::new();
			for i in 0..unsafe { documents.Length() }? {
				let name = unsafe { documents.GetElement(i)?.CurrentName() }?.to_string();
				if !name.trim().is_empty() {
					names.push(name.trim().to_owned());
				}
			}
			Ok(names)
		};
		Ok(windows
			.into_iter()
			.filter_map(|hwnd| match shown(hwnd).as_deref() {
				Ok([name]) => Some(WindowInfo { process: "ChatGPT".to_owned(), title: name.clone(), desktop: desktop_of(hwnd) }),
				_ => None,
			})
			.collect())
	};
	found().unwrap_or_else(|e| {
		log::warn!("could not read ChatGPT windows: {e}");
		Vec::new()
	})
}

/// Something that changes when another window comes to the front or the one
/// in front changes its title: its handle and title. Cheap to ask often.
pub fn foreground_key() -> Option<(isize, String)> {
	let hwnd = unsafe { GetForegroundWindow() };
	if hwnd.0.is_null() {
		return None;
	}
	Some((hwnd.0 as isize, title(hwnd)))
}

/// The window the user is typing into right now.
pub fn foreground() -> Option<WindowInfo> {
	let hwnd = unsafe { GetForegroundWindow() };
	if hwnd.0.is_null() {
		return None;
	}
	Some(info(hwnd))
}

/// The local time of day ("13:02") of a time in ms since the Unix epoch, in
/// the time zone (and daylight saving) Windows has for that date.
pub fn local_clock(ms: u64) -> Option<String> {
	use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
	use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
	// FILETIME counts 100 ns steps since 1601.
	let ticks = ms.checked_mul(10_000)?.checked_add(116_444_736_000_000_000)?;
	let file_time = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
	let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
	// SAFETY: plain conversions between structs on the stack.
	unsafe {
		FileTimeToSystemTime(&file_time, &mut utc).ok()?;
		SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
	}
	Some(format!("{:02}:{:02}", local.wHour, local.wMinute))
}
