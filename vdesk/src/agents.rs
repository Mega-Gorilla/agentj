//! AI agent sessions (Claude Code, Codex), as reported by their hooks.
//!
//! Hooks run `agent-hook.exe`, which posts a trimmed event to a small HTTP
//! listener on 127.0.0.1. Each event moves its session to a state. Names come
//! from the agents' own files (Claude transcript `custom-title`, Codex
//! `session_index.jsonl`), and the virtual desktop from either the Claude
//! window whose title is the session name, or the desktop that was showing
//! when the user submitted a prompt.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::winutil::{self, WindowInfo};

pub const PORT: u16 = 47811;
/// A session still "working" with no event for this long is shown as unknown
/// (Claude Code has no hook for Esc / interrupt).
const STALE_MS: u64 = 15 * 60 * 1000;
/// Sessions with no event for this long are dropped.
const FORGET_MS: u64 = 12 * 60 * 60 * 1000;
/// How often a session's name is re-read from disk.
const NAME_REFRESH_MS: u64 = 10_000;
/// Codex names its automatic approval-review subagents like this.
const CODEX_REVIEWER: &str = "Guardian review";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tool {
	Claude,
	Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
	Idle,
	Working,
	Permission,
	Input,
	Done,
	Error,
}

impl State {
	/// Lower = needs the user sooner.
	pub fn urgency(self) -> u8 {
		match self {
			State::Permission => 0,
			State::Input => 1,
			State::Error => 2,
			State::Done => 3,
			State::Working => 4,
			State::Idle => 5,
		}
	}
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
	pub tool: Tool,
	pub id: String,
	pub name: Option<String>,
	pub cwd: Option<String>,
	pub transcript: Option<String>,
	pub state: State,
	/// When the state last changed (ms since epoch, taken in the hook).
	pub since: u64,
	/// Newest hook time seen; older events arriving late only update metadata.
	pub last: u64,
	/// Virtual desktop GUID.
	pub desktop: Option<String>,
	/// The user has typed into this session since we started tracking it.
	#[serde(default)]
	pub prompted: bool,
	/// Not a session to show (Codex reviewer threads and other subagents).
	#[serde(default)]
	pub hidden: bool,
	#[serde(skip)]
	name_checked: u64,
}

impl Session {
	pub fn label(&self) -> String {
		if let Some(name) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
			return name.trim().to_owned();
		}
		self.cwd
			.as_deref()
			.and_then(|c| std::path::Path::new(c).file_name())
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_else(|| "(名前なし)".to_owned())
	}

	pub fn is_stale(&self, now: u64) -> bool {
		self.state == State::Working && now.saturating_sub(self.last) > STALE_MS
	}

	fn visible(&self) -> bool {
		!self.hidden && (self.name.is_some() || self.prompted)
	}
}

static SESSIONS: LazyLock<Mutex<HashMap<String, Session>>> = LazyLock::new(|| Mutex::new(load()));
static CHANGED: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

pub fn subscribe() -> watch::Receiver<u64> {
	CHANGED.subscribe()
}

fn changed() {
	CHANGED.send_modify(|v| *v = v.wrapping_add(1));
	save();
}

pub fn now_ms() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Sessions to show on the deck.
pub fn visible() -> Vec<Session> {
	SESSIONS.lock().unwrap().values().filter(|s| s.visible()).cloned().collect()
}

// ---------------------------------------------------------------- events

#[derive(Deserialize)]
struct Event {
	hook_event_name: String,
	session_id: String,
	cwd: Option<String>,
	transcript_path: Option<String>,
	notification_type: Option<String>,
	session_title: Option<String>,
	agent_id: Option<String>,
	agent_type: Option<String>,
	#[serde(default)]
	ts: u64,
}

fn next_state(event: &Event) -> Option<State> {
	match event.hook_event_name.as_str() {
		"SessionStart" => Some(State::Idle),
		"UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PreCompact" | "PostCompact"
		| "SubagentStart" | "PreModelRequest" => Some(State::Working),
		"PermissionRequest" => Some(State::Permission),
		"Notification" => match event.notification_type.as_deref() {
			Some("permission_prompt") => Some(State::Permission),
			Some("elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input") => Some(State::Input),
			Some("idle_prompt") => Some(State::Done),
			_ => None,
		},
		"Stop" => Some(State::Done),
		"StopFailure" => Some(State::Error),
		"Interrupt" => Some(State::Idle),
		_ => None,
	}
}

/// Programs a prompt for this tool is typed into. A prompt submitted while
/// something else is in front (e.g. sent from a phone) does not move the
/// session to the current desktop.
fn typed_from(tool: Tool, process: &str) -> bool {
	let own: &[&str] = match tool {
		Tool::Claude => &["claude"],
		Tool::Codex => &["ChatGPT", "Codex"],
	};
	let terminals = ["WindowsTerminal", "Code", "cmd", "powershell", "pwsh", "conhost", "OpenConsole", "wezterm-gui"];
	own.iter().chain(terminals.iter()).any(|p| p.eq_ignore_ascii_case(process))
}

fn handle(tool: Tool, mut event: Event, foreground: Option<WindowInfo>) {
	if event.ts == 0 {
		event.ts = now_ms();
	}
	let key = format!("{tool:?}:{}", event.session_id);
	let mut sessions = SESSIONS.lock().unwrap();

	if event.hook_event_name == "SessionEnd" {
		if let Some(s) = sessions.remove(&key) {
			log::info!("{tool:?} {}: ended", s.label());
		}
		drop(sessions);
		changed();
		return;
	}
	// Codex reports its subagents (e.g. the approval reviewer) with agent_*.
	// Claude reports subagent work inside the parent session, which is fine.
	if tool == Tool::Codex && (event.agent_id.is_some() || event.agent_type.is_some()) {
		return;
	}

	let session = sessions.entry(key).or_insert_with(|| Session {
		tool,
		id: event.session_id.clone(),
		name: None,
		cwd: None,
		transcript: None,
		state: State::Idle,
		since: event.ts,
		last: 0,
		desktop: None,
		prompted: false,
		hidden: false,
		name_checked: 0,
	});
	if let Some(cwd) = event.cwd.take() {
		session.cwd = Some(cwd);
	}
	if let Some(path) = event.transcript_path.take() {
		session.transcript = Some(path);
	}
	if let Some(title) = event.session_title.take().filter(|t| !t.trim().is_empty()) {
		session.name = Some(title);
	}
	if event.hook_event_name == "UserPromptSubmit" {
		session.prompted = true;
		session.name_checked = 0; // a first prompt is when names usually appear
		if let Some(fg) = foreground.filter(|w| typed_from(tool, &w.process))
			&& fg.desktop.is_some()
		{
			session.desktop = fg.desktop;
		}
	}
	if event.ts >= session.last {
		session.last = event.ts;
		if let Some(state) = next_state(&event)
			&& state != session.state
		{
			log::info!("{tool:?} {}: {:?} -> {state:?} ({})", session.label(), session.state, event.hook_event_name);
			session.state = state;
			session.since = event.ts;
		}
	}
	drop(sessions);
	changed();
}

// ---------------------------------------------------------------- listener

pub async fn serve() {
	let listener = match TcpListener::bind(("127.0.0.1", PORT)).await {
		Ok(l) => l,
		Err(e) => {
			log::error!("agent listener could not start on port {PORT}: {e}");
			return;
		}
	};
	log::info!("agent listener on 127.0.0.1:{PORT}");
	loop {
		match listener.accept().await {
			Ok((socket, _)) => {
				tokio::spawn(async move {
					if let Err(e) = connection(socket).await {
						log::debug!("hook connection: {e}");
					}
				});
			}
			Err(e) => log::warn!("accept failed: {e}"),
		}
	}
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
	haystack.windows(needle.len()).position(|w| w == needle)
}

async fn read_some(socket: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<usize> {
	let mut chunk = [0u8; 8192];
	let n = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut chunk))
		.await
		.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))??;
	buf.extend_from_slice(&chunk[..n]);
	Ok(n)
}

async fn connection(mut socket: TcpStream) -> std::io::Result<()> {
	let mut buf = Vec::with_capacity(4096);
	let header_end = loop {
		if read_some(&mut socket, &mut buf).await? == 0 {
			return Ok(());
		}
		if let Some(p) = find(&buf, b"\r\n\r\n") {
			break p + 4;
		}
		if buf.len() > 64 * 1024 {
			return Ok(());
		}
	};
	let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
	let mut lines = head.lines();
	let path = lines.next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_owned();
	let length = lines
		.find_map(|l| {
			let (k, v) = l.split_once(':')?;
			k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse::<usize>().ok())?
		})
		.unwrap_or(0)
		.min(1 << 20);
	while buf.len() < header_end + length {
		if read_some(&mut socket, &mut buf).await? == 0 {
			break;
		}
	}
	socket.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;

	let tool = match path.as_str() {
		"/claude" => Tool::Claude,
		"/codex" => Tool::Codex,
		_ => return Ok(()),
	};
	let body = &buf[header_end..(header_end + length).min(buf.len())];
	let Ok(event) = serde_json::from_slice::<Event>(body) else {
		log::debug!("ignored a hook body that did not parse");
		return Ok(());
	};
	let foreground = if event.hook_event_name == "UserPromptSubmit" {
		let fg = tokio::task::spawn_blocking(winutil::foreground).await.ok().flatten();
		match &fg {
			Some(w) => log::info!("{tool:?} prompt typed into {:?} ({:?}) on desktop {:?}", w.process, w.title, w.desktop),
			None => log::info!("{tool:?} prompt: no foreground window"),
		}
		fg
	} else {
		None
	};
	handle(tool, event, foreground);
	Ok(())
}

// ---------------------------------------------------------------- upkeep

/// Last `customTitle` in the tail of a Claude Code transcript.
fn claude_title(path: &str) -> Option<String> {
	const TAIL: u64 = 1 << 20;
	let mut file = std::fs::File::open(path).ok()?;
	let len = file.metadata().ok()?.len();
	file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
	let mut buf = Vec::new();
	file.read_to_end(&mut buf).ok()?;
	let text = String::from_utf8_lossy(&buf);
	let at = text.rfind("{\"type\":\"custom-title\"")?;
	let line = text[at..].lines().next()?;
	let value: serde_json::Value = serde_json::from_str(line).ok()?;
	value.get("customTitle")?.as_str().map(str::to_owned).filter(|t| !t.trim().is_empty())
}

fn codex_home() -> PathBuf {
	std::env::var_os("CODEX_HOME")
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".codex"))
}

/// Codex thread id -> latest thread name.
fn codex_names() -> HashMap<String, String> {
	let mut names = HashMap::new();
	let Ok(text) = std::fs::read_to_string(codex_home().join("session_index.jsonl")) else {
		return names;
	};
	for line in text.lines() {
		let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
		if let (Some(id), Some(name)) = (v.get("id").and_then(|x| x.as_str()), v.get("thread_name").and_then(|x| x.as_str())) {
			names.insert(id.to_owned(), name.to_owned());
		}
	}
	names
}

/// Periodic work: refresh names, place Claude sessions by their window, drop
/// old sessions. Blocking file and window calls run off the async threads.
pub async fn maintain() {
	let now = now_ms();
	let (claude_due, codex_due, removed) = {
		let mut sessions = SESSIONS.lock().unwrap();
		let before = sessions.len();
		sessions.retain(|_, s| now.saturating_sub(s.last.max(s.since)) < FORGET_MS);
		let removed = sessions.len() != before;
		let due = |s: &Session| now.saturating_sub(s.name_checked) >= NAME_REFRESH_MS;
		let claude: Vec<(String, String)> = sessions
			.iter()
			.filter(|(_, s)| s.tool == Tool::Claude && due(s))
			.filter_map(|(k, s)| Some((k.clone(), s.transcript.clone()?)))
			.collect();
		let codex = sessions.values().any(|s| s.tool == Tool::Codex && due(s));
		(claude, codex, removed)
	};

	let found = tokio::task::spawn_blocking(move || {
		let titles: Vec<(String, Option<String>)> = claude_due.into_iter().map(|(k, p)| (k, claude_title(&p))).collect();
		let codex = if codex_due { Some(codex_names()) } else { None };
		let windows = winutil::app_windows(&["claude"]);
		(titles, codex, windows)
	})
	.await;
	let Ok((titles, codex, windows)) = found else { return };

	let mut any = removed;
	{
		let mut sessions = SESSIONS.lock().unwrap();
		for (key, title) in titles {
			if let Some(s) = sessions.get_mut(&key) {
				s.name_checked = now;
				if title.is_some() && title != s.name {
					s.name = title;
					any = true;
				}
			}
		}
		if let Some(names) = codex {
			for s in sessions.values_mut().filter(|s| s.tool == Tool::Codex) {
				s.name_checked = now;
				if let Some(name) = names.get(&s.id) {
					let hidden = name == CODEX_REVIEWER;
					if s.name.as_deref() != Some(name) || s.hidden != hidden {
						s.name = Some(name.clone());
						s.hidden = hidden;
						any = true;
					}
				}
			}
		}
		// A Claude window titled with the session name says exactly where it is.
		for s in sessions.values_mut().filter(|s| s.tool == Tool::Claude) {
			let Some(name) = s.name.as_deref() else { continue };
			if let Some(w) = windows.iter().find(|w| w.title == name && w.desktop.is_some())
				&& s.desktop != w.desktop
			{
				s.desktop = w.desktop.clone();
				any = true;
			}
		}
	}
	if any {
		changed();
	}
}

// ---------------------------------------------------------------- persistence

fn store_path() -> Option<PathBuf> {
	std::env::current_exe().ok().map(|p| p.with_file_name("agents.json"))
}

fn load() -> HashMap<String, Session> {
	let now = now_ms();
	store_path()
		.and_then(|p| std::fs::read(p).ok())
		.and_then(|b| serde_json::from_slice::<HashMap<String, Session>>(&b).ok())
		.map(|m| m.into_iter().filter(|(_, s)| now.saturating_sub(s.last.max(s.since)) < FORGET_MS).collect())
		.unwrap_or_default()
}

fn save() {
	let Some(path) = store_path() else { return };
	let json = {
		let sessions = SESSIONS.lock().unwrap();
		serde_json::to_vec_pretty(&*sessions)
	};
	if let Ok(json) = json {
		let _ = std::fs::write(path, json);
	}
}
