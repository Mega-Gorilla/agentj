//! The agent part: AI agent sessions (Claude Code, Codex), as reported by
//! their hooks, and the keys that show them and jump to them (`keys`). It
//! uses the desktop part only through `desktops::get()`.
//!
//! Hooks run `agentj-hook.exe`, which posts a trimmed event to a small HTTP
//! listener on 127.0.0.1. [`Tracker`] turns the events into session states; it
//! does no I/O, so its rules are unit tested. Hooks run in the background and
//! are not delivered in order, so every rule goes by the time taken in the
//! hook (`ts`), not by arrival.
//!
//! Approval requests are tracked per tool call: a request stays open until
//! that call finishes or the turn ends, whatever other calls do meanwhile.
//! Neither agent reports "approved". With Codex automatic review the request
//! goes to a reviewer, not the user, and nothing in the hook says which, so
//! the config file decides how it is shown (see [`State::Reviewing`]).
//!
//! Names come from the agents' own files (Claude transcript `custom-title`,
//! Codex `session_index.jsonl`), and the virtual desktop from either the
//! Claude window whose title is the session name, or the window that was in
//! front when the user sent a prompt (`front`). A Codex thread with neither
//! borrows the desktop of the Claude session with the same name. An ended
//! session is kept for a while, unseen, so that a resumed one still has its
//! name and desktop.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

pub mod front;
pub mod hooks;
pub mod keys;
mod layout;
mod prefs;
mod render;
pub mod winutil;

use self::winutil::WindowInfo;

pub const PORT: u16 = 47811;
/// A session still "working" with no event for this long is shown as unknown
/// (Claude Code has no hook for Esc / interrupt).
const STALE_MS: u64 = 15 * 60 * 1000;
/// Sessions with no event for this long are dropped.
const FORGET_MS: u64 = 12 * 60 * 60 * 1000;
/// How often a session's name is re-read from disk.
const NAME_REFRESH_MS: u64 = 10_000;
/// An automatic review takes seconds. A request open for longer than this is
/// shown as waiting for the user after all: the thread may not be using the
/// reviewer the config file names.
const REVIEW_OVERDUE_MS: u64 = 2 * 60 * 1000;
/// Per tool: how long before its hook a prompt was sent, roughly, and how far
/// back from the hook to look at all for the window it was typed into. Codex
/// runs the hook 1.5 to 2 s after the prompt is sent.
const PROMPT_TIMING_MS: [(Tool, u64, u64); 2] = [(Tool::Claude, 0, 1_500), (Tool::Codex, 1_800, 5_000)];
/// The refresh key removes a Codex session with no known desktop once it has
/// been quiet for this long.
const SWEEP_IDLE_MS: u64 = 30 * 60 * 1000;
/// Codex runs the calls of one batch at once, so they start and make their
/// approval requests within moments of each other; the next batch only starts
/// after every call has come back and the model has answered again, which
/// takes longer than this.
const BATCH_GAP_MS: u64 = 1_000;
/// Codex: whether what happened at `at` is of an earlier batch than a call
/// that started at `latest`.
fn earlier_batch(at: u64, latest: u64) -> bool {
	at.saturating_add(BATCH_GAP_MS) < latest
}
/// Finished calls remembered per session, for requests that arrive after them.
const FINISHED_KEEP: usize = 16;
/// Codex names its automatic approval-review subagents like this.
const CODEX_REVIEWER: &str = "Guardian review";
/// Title of a Claude desktop window that does not show its session's name.
const APP_TITLE: &str = "Claude";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tool {
	Claude,
	Codex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
	Idle,
	Working,
	/// Codex: an approval request is with the automatic reviewer. Nothing for
	/// the user to do.
	Reviewing,
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
			State::Reviewing => 4,
			State::Working => 5,
			State::Idle => 6,
		}
	}

	/// The user has to do something before the session can go on.
	pub fn needs_user(self) -> bool {
		matches!(self, State::Permission | State::Input | State::Error)
	}
}

/// A session as shown on the deck.
#[derive(Clone, Debug)]
pub struct View {
	pub tool: Tool,
	pub label: String,
	pub state: State,
	/// "Working" with no sign of life for a long time.
	pub stale: bool,
	/// When it entered `state` (ms since epoch).
	pub since: u64,
	/// Virtual desktop GUID, if known.
	pub desktop: Option<String>,
}

/// What a recorded desktop rests on, weakest first. A prompt only moves a
/// session to another desktop on evidence at least as strong as what put it
/// where it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum Evidence {
	/// Recorded before this was kept, or not known.
	#[default]
	Unknown,
	/// A prompt sent while a terminal was in front. Many terminals are not
	/// the agent (an SSH session, a build), so this is easily wrong.
	Terminal,
	/// The agent's own app: a prompt sent from it, or its window.
	App,
}

/// An approval request that has not been answered as far as we know.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Ask {
	/// Hash of the tool call it is for; 0 = unknown.
	call: u64,
	at: u64,
}

/// A tool call that has started and not finished.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Call {
	id: String,
	at: u64,
}

/// A tool call that finished without an approval request being open for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Finished {
	id: Option<String>,
	call: u64,
	at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Session {
	tool: Tool,
	id: String,
	name: Option<String>,
	cwd: Option<String>,
	transcript: Option<String>,
	/// What the events say apart from approvals (never Permission / Reviewing;
	/// those come from `asks`, see `shown`).
	state: State,
	/// When `state` last changed (ms since epoch, taken in the hook).
	since: u64,
	/// Newest hook time seen; older events arriving late do not change `state`.
	last: u64,
	/// Virtual desktop GUID.
	desktop: Option<String>,
	/// What `desktop` rests on.
	#[serde(default)]
	desktop_from: Evidence,
	/// The user has typed into this session since we started tracking it.
	#[serde(default)]
	prompted: bool,
	/// Not a session to show (Codex reviewer threads and other subagents).
	#[serde(default)]
	hidden: bool,
	#[serde(default)]
	asks: Vec<Ask>,
	#[serde(default)]
	open: Vec<Call>,
	#[serde(default)]
	finished: Vec<Finished>,
	/// When a turn or a batch of calls last ended: approval requests from
	/// before then are settled, also ones that arrive late.
	#[serde(default)]
	settled: u64,
	/// Codex: when the latest tool call started (see `BATCH_GAP_MS`).
	#[serde(default)]
	latest_start: u64,
	/// When the session ended (or was cleared away by the refresh key). It
	/// is kept, unseen, so that events from before then are dropped and so
	/// that its name and desktop are still known if it is resumed.
	#[serde(default)]
	ended: Option<u64>,
	#[serde(skip)]
	name_checked: u64,
	/// How far the transcript has been searched for a title.
	#[serde(skip)]
	title_offset: u64,
}

impl Session {
	fn new(tool: Tool, id: &str, ts: u64) -> Self {
		Session {
			tool,
			id: id.to_owned(),
			name: None,
			cwd: None,
			transcript: None,
			state: State::Idle,
			since: ts,
			last: 0,
			desktop: None,
			desktop_from: Evidence::Unknown,
			prompted: false,
			hidden: false,
			asks: Vec::new(),
			open: Vec::new(),
			finished: Vec::new(),
			settled: 0,
			latest_start: 0,
			ended: None,
			name_checked: 0,
			title_offset: 0,
		}
	}

	/// The session is over as of `ts`; see `ended`.
	fn end(&mut self, ts: u64) {
		self.ended = Some(ts);
		self.settle(ts);
	}

	/// Put the session on `desktop`, unless what put it where it is was
	/// stronger evidence. Returns whether it moved.
	fn place(&mut self, desktop: &str, evidence: Evidence) -> bool {
		if evidence < self.desktop_from {
			return false;
		}
		self.desktop_from = evidence;
		if self.desktop.as_deref() == Some(desktop) {
			return false;
		}
		self.desktop = Some(desktop.to_owned());
		true
	}

	fn label(&self) -> String {
		if let Some(name) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
			return name.trim().to_owned();
		}
		self.cwd
			.as_deref()
			.and_then(|c| std::path::Path::new(c).file_name())
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_else(|| "(名前なし)".to_owned())
	}

	fn is_stale(&self, now: u64) -> bool {
		self.state == State::Working && self.asks.is_empty() && now.saturating_sub(self.last) > STALE_MS
	}

	fn visible(&self) -> bool {
		self.ended.is_none() && !self.hidden && (self.name.is_some() || self.prompted)
	}

	/// The state to show and when it began. An open approval request wins
	/// over whatever else the session is doing.
	fn shown(&self, now: u64, codex_auto_review: bool) -> (State, u64) {
		match self.asks.iter().map(|a| a.at).min() {
			Some(at) if self.tool == Tool::Codex && codex_auto_review && now.saturating_sub(at) < REVIEW_OVERDUE_MS => {
				(State::Reviewing, at)
			}
			Some(at) => (State::Permission, at),
			None => (self.state, self.since),
		}
	}

	/// Everything up to `ts` is over: no request or call from before then is
	/// pending, also if its event arrives later.
	fn settle(&mut self, ts: u64) {
		self.settled = self.settled.max(ts);
		self.asks.retain(|a| a.at > ts);
		self.open.retain(|c| c.at > ts);
		self.finished.retain(|f| f.at > ts);
	}

	/// Codex: what happened at `at` (a call starting, a request) belongs to a
	/// batch that is over, because a call of a later batch has started.
	fn batch_over(&self, at: u64) -> bool {
		self.tool == Tool::Codex && earlier_batch(at, self.latest_start)
	}

	/// Follow tool calls and the approval requests made for them.
	///
	/// Order within one call is PreToolUse, PermissionRequest (only when it
	/// needs approval), then PostToolUse once it has run; hooks run in the
	/// background, so they may arrive in any order. Only the Pre and Post
	/// events carry the call's id, so a request is matched to its call by a
	/// hash of the tool input (`Event::call`).
	///
	/// A request is pending until one of:
	/// - its call finishes (PostToolUse / PostToolUseFailure);
	/// - the batch or the turn ends (`settle`);
	/// - Codex only: a call of a later batch starts. Codex sends nothing when
	///   a request is rejected, and the model takes its next step only once
	///   every call of the batch has come back (`batch_over`).
	fn track_calls(&mut self, event: &Event) {
		let (ts, call) = (event.ts, event.call.unwrap_or(0));
		match event.hook_event_name.as_str() {
			"PreToolUse" => {
				let Some(id) = event.tool_use_id.as_deref() else { return };
				let known = self.open.iter().any(|c| c.id == id) || self.finished.iter().any(|f| f.id.as_deref() == Some(id));
				if !known && ts > self.settled {
					self.open.push(Call { id: id.to_owned(), at: ts });
				}
				if self.tool == Tool::Codex && ts > self.latest_start {
					// Maybe a new batch: what earlier ones asked is settled.
					self.latest_start = ts;
					self.asks.retain(|a| !earlier_batch(a.at, ts));
					self.open.retain(|c| !earlier_batch(c.at, ts));
				}
			}
			"PostToolUse" | "PostToolUseFailure" => {
				if let Some(id) = event.tool_use_id.as_deref() {
					self.open.retain(|c| c.id != id);
				}
				match self.asks.iter().position(|a| call != 0 && a.call == call && a.at <= ts) {
					Some(i) => {
						self.asks.remove(i);
					}
					// Its request, if any, has not arrived yet.
					None if ts > self.settled => {
						self.finished.push(Finished { id: event.tool_use_id.clone(), call, at: ts });
						if self.finished.len() > FINISHED_KEEP {
							self.finished.remove(0);
						}
					}
					None => {}
				}
			}
			"PermissionRequest" => {
				if ts <= self.settled || self.batch_over(ts) {
					return; // late: its turn or batch is over already
				}
				match self.finished.iter().position(|f| call != 0 && f.call == call && f.at >= ts) {
					Some(i) => {
						self.finished.remove(i);
					}
					None => self.asks.push(Ask { call, at: ts }),
				}
			}
			// Claude Code's reminder, about 6 s into an unanswered prompt:
			// stands in for a request that did not arrive. Not known which
			// call it is for, so only the end of the batch settles it.
			"Notification" if event.notification_type.as_deref() == Some("permission_prompt") => {
				if self.asks.is_empty() && ts > self.settled {
					self.asks.push(Ask { call: 0, at: ts });
				}
			}
			"Stop" | "StopFailure" | "Interrupt" | "SessionStart" | "UserPromptSubmit" | "PostToolBatch" => self.settle(ts),
			_ => {}
		}
	}
}

// ---------------------------------------------------------------- events

#[derive(Default, Deserialize)]
struct Event {
	hook_event_name: String,
	session_id: String,
	cwd: Option<String>,
	transcript_path: Option<String>,
	notification_type: Option<String>,
	session_title: Option<String>,
	agent_id: Option<String>,
	agent_type: Option<String>,
	tool_use_id: Option<String>,
	/// SessionStart: why the session started (startup, resume, compact...).
	source: Option<String>,
	/// Hash of the tool call's input, added by agentj-hook.
	call: Option<u64>,
	#[serde(default)]
	ts: u64,
}

/// What an event says the session is doing, apart from approvals.
fn next_state(event: &Event) -> Option<State> {
	match event.hook_event_name.as_str() {
		"SessionStart" => Some(State::Idle),
		"UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure" | "PostToolBatch" | "PreCompact"
		| "PostCompact" | "SubagentStart" => Some(State::Working),
		// agent_needs_input is left out: it also carries auto-mode classifier
		// notices that need nobody.
		"Notification" => match event.notification_type.as_deref() {
			Some("elicitation_dialog" | "elicitation_url_dialog") => Some(State::Input),
			Some("idle_prompt") => Some(State::Done),
			_ => None,
		},
		"Stop" => Some(State::Done),
		"StopFailure" => Some(State::Error),
		"Interrupt" => Some(State::Idle),
		_ => None,
	}
}

/// Whether a prompt for this tool could have been typed into a window of
/// `process`, and how much that says (see `Evidence`). A prompt submitted
/// while something else is in front (e.g. sent from a phone) says nothing.
fn typed_from(tool: Tool, process: &str) -> Option<Evidence> {
	let own: &[&str] = match tool {
		Tool::Claude => &["claude"],
		Tool::Codex => &["ChatGPT", "Codex"],
	};
	let terminals = ["WindowsTerminal", "Code", "cmd", "powershell", "pwsh", "conhost", "OpenConsole", "wezterm-gui"];
	let is = |names: &[&str]| names.iter().any(|p| p.eq_ignore_ascii_case(process));
	if is(own) {
		Some(Evidence::App)
	} else if is(&terminals) {
		Some(Evidence::Terminal)
	} else {
		None
	}
}

/// Claude Code holds a prompt sent while it is busy and runs its hook only
/// when it takes the prompt in, by when the user may well be on another
/// desktop. (Codex takes such a prompt in at once: its hook comes while the
/// window it was typed into is still around.)
fn hook_waits_for_busy_session(tool: Tool) -> bool {
	tool == Tool::Claude
}

fn key(tool: Tool, session_id: &str) -> String {
	format!("{tool:?}:{session_id}")
}

/// All sessions and the rules that move them. No I/O.
#[derive(Default)]
struct Tracker {
	/// Live sessions, and ended ones kept for a while (`Session::ended`).
	sessions: HashMap<String, Session>,
}

impl Tracker {
	/// Apply one hook event. Returns whether anything may have changed.
	///
	/// `recent` is the windows that were in front around the time of a
	/// prompt, likeliest first (see `front::around`).
	fn apply(&mut self, tool: Tool, mut event: Event, recent: &[WindowInfo], codex_auto_review: bool) -> bool {
		// Codex reports its subagents with agent_*, under the parent's session
		// id. Claude reports subagent work inside the parent session, which is fine.
		if tool == Tool::Codex && (event.agent_id.is_some() || event.agent_type.is_some()) {
			return false;
		}
		let key = key(tool, &event.session_id);
		if self.sessions.get(&key).and_then(|s| s.ended).is_some_and(|ended| event.ts <= ended) {
			return false; // from before the session ended
		}
		if event.hook_event_name == "SessionEnd" {
			let session = self.sessions.entry(key).or_insert_with(|| Session::new(tool, &event.session_id, event.ts));
			if event.ts < session.last {
				log::info!("{tool:?} {}: an old SessionEnd arrived late, ignored", session.label());
				return false; // the session has gone on since
			}
			if session.ended.is_none() {
				log::info!("{tool:?} {}: ended", session.label());
			}
			session.end(event.ts);
			return true;
		}

		// A window titled with another of this tool's sessions is that
		// session's, not this one's.
		let others: Vec<String> = self
			.sessions
			.iter()
			.filter(|(k, s)| **k != key && s.tool == tool && s.ended.is_none())
			.filter_map(|(_, s)| s.name.clone())
			.collect();
		let session = self.sessions.entry(key).or_insert_with(|| Session::new(tool, &event.session_id, event.ts));
		if let Some(ended) = session.ended.take() {
			// Resumed: a fresh start, but its name and desktop are still good.
			log::info!("{tool:?} {}: resumed", session.label());
			session.state = State::Idle;
			session.since = event.ts;
			session.last = ended;
		}
		let before = session.shown(event.ts, codex_auto_review).0;

		if let Some(cwd) = event.cwd.take() {
			session.cwd = Some(cwd);
		}
		if let Some(path) = event.transcript_path.take() {
			session.transcript = Some(path);
		}
		if let Some(title) = event.session_title.take().filter(|t| !t.trim().is_empty()) {
			session.name = Some(title);
		}
		if event.hook_event_name == "SessionStart" {
			let source = event.source.as_deref().unwrap_or("?");
			log::info!("{tool:?} {}: session start ({source})", session.label());
		}
		if event.hook_event_name == "UserPromptSubmit" {
			session.prompted = true;
			session.name_checked = 0; // a first prompt is when names usually appear
			let busy = matches!(session.state, State::Working | State::Input) || !session.asks.is_empty();
			let held = hook_waits_for_busy_session(tool) && busy && !session.is_stale(event.ts);
			// The hook comes a moment after the prompt was sent (about 2 s for
			// Codex), and the user may have clicked another window since, so
			// any window in front lately that the prompt could have been typed
			// into will do, the likeliest first.
			let typed_in = recent.iter().find_map(|w| {
				let evidence = typed_from(tool, &w.process)?;
				let desktop = w.desktop.as_deref()?;
				(!others.contains(&w.title)).then_some((desktop, evidence, w))
			});
			let label = session.label();
			match (typed_in, recent.first()) {
				(_, Some(front)) if held => log::info!("{tool:?} {label}: prompt held while busy, desktop of {:?} not used", front.title),
				(Some((desktop, evidence, window)), _) => {
					if !session.place(desktop, evidence) && session.desktop.as_deref() != Some(desktop) {
						log::info!(
							"{tool:?} {label}: {:?} {:?} is weaker evidence than what placed it, desktop kept",
							window.process,
							window.title
						);
					}
				}
				(None, Some(front)) => log::info!(
					"{tool:?} {label}: no window it could be typed into in front lately ({:?} {:?}), desktop not recorded",
					front.process,
					front.title
				),
				(None, None) => {}
			}
		}

		session.track_calls(&event);
		if event.ts >= session.last {
			session.last = event.ts;
			if let Some(state) = next_state(&event)
				&& state != session.state
			{
				session.state = state;
				session.since = event.ts;
			}
		}
		let after = session.shown(event.ts, codex_auto_review).0;
		if after != before {
			log::info!("{tool:?} {}: {before:?} -> {after:?} ({})", session.label(), event.hook_event_name);
		}
		true
	}

	/// Sessions to show on the deck.
	fn views(&self, now: u64, codex_auto_review: bool) -> Vec<View> {
		self.sessions
			.values()
			.filter(|s| s.visible())
			.map(|s| {
				let (state, since) = s.shown(now, codex_auto_review);
				let desktop = s.desktop.clone().or_else(|| self.namesake_desktop(s));
				View { tool: s.tool, label: s.label(), state, stale: s.is_stale(now), since, desktop }
			})
			.collect()
	}

	/// Every ChatGPT window is titled just "ChatGPT", so a Codex thread that
	/// has not had a prompt typed since we started tracking it borrows the
	/// desktop of the Claude session with the same name (the user names a
	/// project's Claude session and Codex thread alike). Not stored, so it
	/// follows the Claude window if it moves; none if same-named sessions sit
	/// on different desktops.
	fn namesake_desktop(&self, codex: &Session) -> Option<String> {
		if codex.tool != Tool::Codex {
			return None;
		}
		let name = codex.name.as_deref()?.trim();
		let mut desktops = self
			.sessions
			.values()
			.filter(|s| s.tool == Tool::Claude && !s.hidden && s.ended.is_none())
			.filter(|s| s.name.as_deref().is_some_and(|n| n.trim().eq_ignore_ascii_case(name)))
			.filter_map(|s| s.desktop.as_deref());
		let first = desktops.next()?;
		desktops.all(|d| d == first).then(|| first.to_owned())
	}

	/// Put `tool`'s sessions on the desktop of the app window that shows them
	/// by name (`WindowInfo::title`): a Claude window titled with the session
	/// name, a ChatGPT window showing the Codex thread. Not when windows with
	/// that name sit on different desktops: then it is not known which is
	/// whose, and what is recorded stays. Returns whether any moved.
	fn place_by_names(&mut self, tool: Tool, windows: &[WindowInfo]) -> bool {
		let mut any = false;
		for s in self.sessions.values_mut().filter(|s| s.tool == tool && s.ended.is_none()) {
			let Some(name) = s.name.as_deref() else { continue };
			let mut desktops = windows.iter().filter(|w| w.title == name).filter_map(|w| w.desktop.as_ref());
			let Some(first) = desktops.next() else { continue };
			if desktops.all(|d| d == first) && s.place(first, Evidence::App) {
				log::info!("{tool:?} {}: placed by its app window", s.label());
				any = true;
			}
		}
		any
	}

	/// Put Claude sessions on the desktop of their window. Returns whether
	/// any moved.
	fn place_by_windows(&mut self, windows: &[WindowInfo]) -> bool {
		let mut any = self.place_by_names(Tool::Claude, windows);
		// Some windows are titled just "Claude" instead of their session's
		// name. With exactly one of those and exactly one session that has no
		// window of its own, they go together.
		let untitled: Vec<&WindowInfo> = windows.iter().filter(|w| w.title == APP_TITLE && w.desktop.is_some()).collect();
		let has_window = |s: &Session| s.name.as_deref().is_some_and(|n| windows.iter().any(|w| w.title == n));
		let mut unplaced = self.sessions.values_mut().filter(|s| s.tool == Tool::Claude && s.visible() && !has_window(s));
		if let ([window], Some(s), None) = (untitled.as_slice(), unplaced.next(), unplaced.next())
			&& let Some(desktop) = window.desktop.as_deref()
			&& s.place(desktop, Evidence::App)
		{
			log::info!("Claude {}: placed by the window titled {APP_TITLE:?}", s.label());
			any = true;
		}
		any
	}

	/// Remove sessions that look closed, for the refresh key. Returns their
	/// labels.
	///
	/// A closed session does not always send SessionEnd. Sessions that need
	/// the user or are at work stay whatever the windows say. Of the rest:
	/// - Claude goes when no window is titled with its name and no untitled
	///   "Claude" window is on its desktop.
	/// - Codex goes when no ChatGPT window shows its thread (`codex_threads`,
	///   read through UI Automation). When that could not be read, all that
	///   is known is where the windows are (`codex`), and it goes when its
	///   desktop has no ChatGPT window left, or, with no desktop recorded,
	///   when nothing has happened in it for `SWEEP_IDLE_MS`.
	///
	/// They are marked ended, not dropped: one cleared by mistake comes back
	/// with its next hook event, name and desktop intact.
	fn sweep(
		&mut self,
		now: u64,
		codex_auto_review: bool,
		claude: &[WindowInfo],
		codex: &[WindowInfo],
		codex_threads: Option<&[WindowInfo]>,
	) -> Vec<String> {
		let closed = |s: &Session| {
			let state = s.shown(now, codex_auto_review).0;
			if s.ended.is_some() || state.needs_user() || state == State::Reviewing || (state == State::Working && !s.is_stale(now)) {
				return false;
			}
			match s.tool {
				Tool::Claude => {
					let own = s.name.as_deref().is_some_and(|n| claude.iter().any(|w| w.title == n));
					let untitled = s.desktop.is_some() && claude.iter().any(|w| w.title == APP_TITLE && w.desktop == s.desktop);
					!own && !untitled
				}
				Tool::Codex => match (codex_threads, s.name.as_deref(), &s.desktop) {
					(Some(threads), Some(name), _) => !threads.iter().any(|w| w.title == name),
					(_, _, Some(desktop)) => !codex.iter().any(|w| w.desktop.as_ref() == Some(desktop)),
					(_, _, None) => now.saturating_sub(s.last.max(s.since)) > SWEEP_IDLE_MS,
				},
			}
		};
		let mut gone = Vec::new();
		for s in self.sessions.values_mut().filter(|s| closed(s)) {
			gone.push(format!("{:?} {}", s.tool, s.label()));
			s.end(now);
		}
		gone
	}

	/// Forget sessions, live or ended, that are too old. Returns whether a
	/// live one went.
	fn prune(&mut self, now: u64) -> bool {
		let live = |sessions: &HashMap<String, Session>| sessions.values().filter(|s| s.ended.is_none()).count();
		let before = live(&self.sessions);
		self.sessions.retain(|_, s| now.saturating_sub(s.last.max(s.since).max(s.ended.unwrap_or(0))) < FORGET_MS);
		live(&self.sessions) != before
	}
}

// ---------------------------------------------------------------- shared state

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(|| Mutex::new(Tracker { sessions: load() }));
static CHANGED: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);
/// Whether Codex sends approval requests to its automatic reviewer.
static CODEX_AUTO_REVIEW: LazyLock<AtomicBool> = LazyLock::new(|| AtomicBool::new(codex_auto_review()));

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
pub fn visible() -> Vec<View> {
	TRACKER.lock().unwrap().views(now_ms(), CODEX_AUTO_REVIEW.load(Ordering::Relaxed))
}

fn handle(tool: Tool, mut event: Event) {
	if event.ts == 0 {
		event.ts = now_ms();
	}
	// For a prompt: the windows it may have been typed into, likeliest first.
	let recent = if event.hook_event_name == "UserPromptSubmit" {
		let (delay, within) = PROMPT_TIMING_MS.iter().find(|(t, ..)| *t == tool).map_or((0, 0), |(_, d, w)| (*d, *w));
		let recent = front::around(event.ts.saturating_sub(delay), event.ts.saturating_sub(within));
		let seen: Vec<_> = recent.iter().map(|w| (w.process.as_str(), w.title.as_str())).collect();
		log::info!("{tool:?} prompt, in front around then (likeliest first): {seen:?}");
		recent
	} else {
		Vec::new()
	};
	let auto_review = CODEX_AUTO_REVIEW.load(Ordering::Relaxed);
	let any = TRACKER.lock().unwrap().apply(tool, event, &recent, auto_review);
	if any {
		changed();
	}
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
	handle(tool, event);
	Ok(())
}

// ---------------------------------------------------------------- upkeep

/// Newest `customTitle` in a Claude Code transcript from byte `offset` on, and
/// the offset to carry on from next time (after the last complete line). The
/// title can be anywhere in the file, so the first call reads all of it and
/// later calls only what was added.
fn claude_title(path: &str, mut offset: u64) -> (Option<String>, u64) {
	let Ok(file) = std::fs::File::open(path) else { return (None, offset) };
	if file.metadata().map(|m| m.len()).unwrap_or(0) < offset {
		offset = 0; // replaced by a shorter file
	}
	let mut reader = BufReader::with_capacity(1 << 16, file);
	if reader.seek(SeekFrom::Start(offset)).is_err() {
		return (None, offset);
	}
	let mut title = None;
	let mut line = Vec::new();
	loop {
		line.clear();
		match reader.read_until(b'\n', &mut line) {
			Ok(n) if n > 0 && line.ends_with(b"\n") => offset += n as u64,
			_ => break, // end of file, or a line still being written
		}
		if find(&line, b"\"custom-title\"").is_some()
			&& let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line)
			&& value.get("type").and_then(|t| t.as_str()) == Some("custom-title")
			&& let Some(t) = value.get("customTitle").and_then(|t| t.as_str()).filter(|t| !t.trim().is_empty())
		{
			title = Some(t.to_owned());
		}
	}
	(title, offset)
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

/// Whether a Codex `config.toml` sends approval requests to the automatic
/// reviewer: top-level `approvals_reviewer = "auto_review"` (or its old name
/// "guardian_subagent"). The desktop app can also set this per thread, which
/// is not visible here.
fn auto_review_in(config: &str) -> bool {
	for line in config.lines().map(str::trim) {
		if line.starts_with('[') {
			break; // top-level keys end at the first table
		}
		if let Some((name, value)) = line.split_once('=')
			&& name.trim() == "approvals_reviewer"
		{
			let value = value.split('#').next().unwrap_or("").trim().trim_matches(['"', '\'']);
			return matches!(value, "auto_review" | "guardian_subagent");
		}
	}
	false
}

fn codex_auto_review() -> bool {
	std::fs::read_to_string(codex_home().join("config.toml")).is_ok_and(|text| auto_review_in(&text))
}

/// The refresh key: re-read names and windows now, put Codex sessions on the
/// desktop of the ChatGPT window showing them, then remove the sessions that
/// look closed (see `Tracker::sweep`). Returns how many went.
pub async fn sweep() -> usize {
	for s in TRACKER.lock().unwrap().sessions.values_mut() {
		s.name_checked = 0;
	}
	maintain().await;
	let windows = tokio::task::spawn_blocking(|| {
		(winutil::app_windows(&["claude"]), winutil::app_windows(&["ChatGPT", "Codex"]), winutil::chatgpt_threads())
	})
	.await;
	let Ok((claude, codex, threads)) = windows else { return 0 };
	let shown: Vec<_> = threads.iter().map(|w| w.title.as_str()).collect();
	log::info!("refresh: ChatGPT windows show {shown:?}");
	// No thread read from windows that are there: reading failed, so what
	// the windows show is not known.
	let threads_known = codex.is_empty() || !threads.is_empty();
	let auto_review = CODEX_AUTO_REVIEW.load(Ordering::Relaxed);
	let (placed, gone) = {
		let mut tracker = TRACKER.lock().unwrap();
		let placed = tracker.place_by_names(Tool::Codex, &threads);
		(placed, tracker.sweep(now_ms(), auto_review, &claude, &codex, threads_known.then_some(threads.as_slice())))
	};
	for label in &gone {
		log::info!("{label}: looks closed, removed");
	}
	if placed || !gone.is_empty() {
		changed();
	}
	gone.len()
}

/// Periodic work: refresh names, place Claude sessions by their window, drop
/// old sessions. Blocking file and window calls run off the async threads.
pub async fn maintain() {
	let now = now_ms();
	let (claude_due, codex_due, removed) = {
		let mut tracker = TRACKER.lock().unwrap();
		let removed = tracker.prune(now);
		let due = |s: &Session| s.ended.is_none() && now.saturating_sub(s.name_checked) >= NAME_REFRESH_MS;
		let claude: Vec<(String, String, u64)> = tracker
			.sessions
			.iter()
			.filter(|(_, s)| s.tool == Tool::Claude && due(s))
			.filter_map(|(k, s)| Some((k.clone(), s.transcript.clone()?, s.title_offset)))
			.collect();
		let codex = tracker.sessions.values().any(|s| s.tool == Tool::Codex && due(s));
		(claude, codex, removed)
	};

	let found = tokio::task::spawn_blocking(move || {
		let titles: Vec<_> = claude_due.into_iter().map(|(key, path, offset)| (key, claude_title(&path, offset))).collect();
		let codex = codex_due.then(|| (codex_names(), codex_auto_review()));
		let windows = winutil::app_windows(&["claude"]);
		(titles, codex, windows)
	})
	.await;
	let Ok((titles, codex, windows)) = found else { return };

	let mut any = removed;
	{
		let mut tracker = TRACKER.lock().unwrap();
		for (key, (title, offset)) in titles {
			if let Some(s) = tracker.sessions.get_mut(&key) {
				s.name_checked = now;
				s.title_offset = offset;
				if title.is_some() && title != s.name {
					s.name = title;
					any = true;
				}
			}
		}
		if let Some((names, auto_review)) = codex {
			if CODEX_AUTO_REVIEW.swap(auto_review, Ordering::Relaxed) != auto_review {
				log::info!("Codex automatic review: {auto_review}");
				any = true;
			}
			for s in tracker.sessions.values_mut().filter(|s| s.tool == Tool::Codex) {
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
		any |= tracker.place_by_windows(&windows);
	}
	if any {
		changed();
	}
}

// ---------------------------------------------------------------- persistence

fn store_path() -> Option<PathBuf> {
	Some(crate::paths::data_file("agents.json"))
}

fn load() -> HashMap<String, Session> {
	let now = now_ms();
	let mut sessions: HashMap<String, Session> = store_path()
		.and_then(|p| std::fs::read(p).ok())
		.and_then(|b| serde_json::from_slice(&b).ok())
		.unwrap_or_default();
	sessions.retain(|_, s| now.saturating_sub(s.last.max(s.since).max(s.ended.unwrap_or(0))) < FORGET_MS);
	for s in sessions.values_mut() {
		// Files from before approvals were tracked per call kept them here.
		if matches!(s.state, State::Permission | State::Reviewing) {
			s.state = State::Working;
		}
	}
	sessions
}

fn save() {
	let Some(path) = store_path() else { return };
	let json = serde_json::to_vec_pretty(&TRACKER.lock().unwrap().sessions);
	if let Ok(json) = json {
		let _ = std::fs::write(path, json);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const S: &str = "s";

	fn ev(name: &str, ts: u64) -> Event {
		Event { hook_event_name: name.to_owned(), session_id: S.to_owned(), ts, ..Default::default() }
	}

	/// PreToolUse / PostToolUse of call `id` whose input hashes to `call`.
	fn tool(name: &str, ts: u64, id: &str, call: u64) -> Event {
		Event { tool_use_id: Some(id.to_owned()), call: Some(call), ..ev(name, ts) }
	}

	fn ask(ts: u64, call: u64) -> Event {
		Event { call: Some(call), ..ev("PermissionRequest", ts) }
	}

	fn feed(tracker: &mut Tracker, tool: Tool, auto_review: bool, events: Vec<Event>) {
		for event in events {
			tracker.apply(tool, event, &[], auto_review);
		}
	}

	/// Sessions that are not ended.
	fn live(tracker: &Tracker) -> usize {
		tracker.sessions.values().filter(|s| s.ended.is_none()).count()
	}

	fn state(tracker: &Tracker, tool: Tool, now: u64, auto_review: bool) -> State {
		tracker.sessions[&key(tool, S)].shown(now, auto_review).0
	}

	fn window(title: &str, desktop: &str) -> WindowInfo {
		WindowInfo { process: "claude".to_owned(), title: title.to_owned(), desktop: Some(desktop.to_owned()) }
	}

	fn named(tool: Tool, id: &str, name: &str, desktop: Option<&str>) -> Session {
		Session { name: Some(name.to_owned()), desktop: desktop.map(str::to_owned), ..Session::new(tool, id, 0) }
	}

	fn tracker(list: Vec<Session>) -> Tracker {
		Tracker { sessions: list.into_iter().map(|s| (key(s.tool, &s.id), s)).collect() }
	}

	// ---- approvals

	#[test]
	fn another_calls_finish_does_not_clear_a_pending_approval() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![
			tool("PreToolUse", 10, "a", 1),
			tool("PreToolUse", 11, "b", 2),
			ask(12, 1),
			tool("PostToolUse", 20, "b", 2),
		]);
		assert_eq!(state(&t, Tool::Claude, 21, false), State::Permission);
		feed(&mut t, Tool::Claude, false, vec![tool("PostToolUse", 30, "a", 1)]);
		assert_eq!(state(&t, Tool::Claude, 31, false), State::Working);
	}

	#[test]
	fn a_request_arriving_after_its_call_finished_is_not_pending() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1), tool("PostToolUse", 20, "a", 1), ask(12, 1)]);
		assert_eq!(state(&t, Tool::Claude, 21, false), State::Working);
		// the same command again later does ask
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 30, "b", 1), ask(31, 1)]);
		assert_eq!(state(&t, Tool::Claude, 32, false), State::Permission);
	}

	#[test]
	fn the_end_of_the_turn_or_batch_settles_requests() {
		for end in ["Stop", "PostToolBatch", "UserPromptSubmit", "Interrupt"] {
			let mut t = Tracker::default();
			feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1), ask(11, 1), ev(end, 20)]);
			assert!(!state(&t, Tool::Claude, 21, false).needs_user(), "{end}");
		}
		// but not one that ended before the request was made
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1), ask(11, 1), ev("Stop", 5)]);
		assert_eq!(state(&t, Tool::Claude, 21, false), State::Permission);
	}

	#[test]
	fn a_request_matching_no_call_stays_until_the_turn_ends() {
		let mut t = Tracker::default();
		// the request's hash differs from its call's
		feed(&mut t, Tool::Codex, false, vec![tool("PreToolUse", 10, "a", 1), tool("PreToolUse", 10, "b", 2), ask(11, 9)]);
		feed(&mut t, Tool::Codex, false, vec![tool("PostToolUse", 20, "b", 2), tool("PostToolUse", 30, "a", 1)]);
		assert_eq!(state(&t, Tool::Codex, 31, false), State::Permission);
		feed(&mut t, Tool::Codex, false, vec![ev("Stop", 40)]);
		assert_eq!(state(&t, Tool::Codex, 41, false), State::Done);
	}

	#[test]
	fn a_request_from_before_the_turn_ended_arriving_late_is_not_pending() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1), ev("Interrupt", 30), ask(20, 1)]);
		assert!(!state(&t, Tool::Claude, 31, false).needs_user());
		// nor a reminder from then
		let reminder = Event { notification_type: Some("permission_prompt".to_owned()), ..ev("Notification", 25) };
		feed(&mut t, Tool::Claude, false, vec![reminder]);
		assert!(!state(&t, Tool::Claude, 31, false).needs_user());
	}

	#[test]
	fn a_request_whose_call_start_arrives_late_stays_pending() {
		// a's PreToolUse is delayed; b finishes meanwhile
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 11, "b", 2), ask(12, 1), tool("PostToolUse", 20, "b", 2)]);
		assert_eq!(state(&t, Tool::Claude, 21, false), State::Permission);
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1)]);
		assert_eq!(state(&t, Tool::Claude, 22, false), State::Permission);
		feed(&mut t, Tool::Claude, false, vec![tool("PostToolUse", 30, "a", 1)]);
		assert_eq!(state(&t, Tool::Claude, 31, false), State::Working);
	}

	#[test]
	fn claude_reminder_is_settled_by_the_end_of_the_batch_only() {
		// a reminder stands in for a request of unknown call: another call
		// finishing does not answer it
		let mut t = Tracker::default();
		let reminder = Event { notification_type: Some("permission_prompt".to_owned()), ..ev("Notification", 17) };
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1), tool("PreToolUse", 10, "b", 2), reminder]);
		feed(&mut t, Tool::Claude, false, vec![tool("PostToolUse", 20, "b", 2)]);
		assert_eq!(state(&t, Tool::Claude, 21, false), State::Permission);
		feed(&mut t, Tool::Claude, false, vec![ev("PostToolBatch", 30)]);
		assert_eq!(state(&t, Tool::Claude, 31, false), State::Working);
	}

	#[test]
	fn claude_reminder_counts_once() {
		let mut t = Tracker::default();
		let reminder = |ts| Event { notification_type: Some("permission_prompt".to_owned()), ..ev("Notification", ts) };
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 10, "a", 1), ask(11, 1), reminder(17)]);
		feed(&mut t, Tool::Claude, false, vec![tool("PostToolUse", 20, "a", 1)]);
		assert_eq!(state(&t, Tool::Claude, 21, false), State::Working);
		// and stands in for a request that was never received
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 30, "b", 2), reminder(37)]);
		assert_eq!(state(&t, Tool::Claude, 38, false), State::Permission);
	}

	#[test]
	fn codex_automatic_review_is_not_waiting_for_the_user() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Codex, true, vec![tool("PreToolUse", 1_000, "a", 1), ask(1_001, 1)]);
		assert_eq!(state(&t, Tool::Codex, 4_000, true), State::Reviewing);
		assert!(!State::Reviewing.needs_user());
		// far longer than a review takes: maybe the user is being asked after all
		assert_eq!(state(&t, Tool::Codex, 1_001 + REVIEW_OVERDUE_MS, true), State::Permission);
		// without automatic review it is the user's from the start
		assert_eq!(state(&t, Tool::Codex, 4_000, false), State::Permission);
	}

	#[test]
	fn codex_rejected_request_goes_when_the_next_batch_starts() {
		// A rejection sends no event; a call of the next batch shows the model got its answer.
		let mut t = Tracker::default();
		feed(&mut t, Tool::Codex, true, vec![tool("PreToolUse", 1_000, "a", 1), ask(1_001, 1), tool("PreToolUse", 9_000, "b", 2)]);
		assert_eq!(state(&t, Tool::Codex, 9_001, true), State::Working);
		// its request arriving after that is late, not pending
		feed(&mut t, Tool::Codex, true, vec![tool("PreToolUse", 20_000, "c", 3), tool("PreToolUse", 30_000, "d", 4), ask(20_001, 3)]);
		assert_eq!(state(&t, Tool::Codex, 30_001, true), State::Working);
	}

	#[test]
	fn codex_calls_of_the_same_batch_do_not_settle_each_other() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Codex, true, vec![tool("PreToolUse", 1_000, "a", 1), ask(1_001, 1), tool("PreToolUse", 1_200, "b", 2)]);
		assert_eq!(state(&t, Tool::Codex, 4_000, true), State::Reviewing);
		// and it still turns into waiting for the user when the review takes too long
		assert_eq!(state(&t, Tool::Codex, 1_001 + REVIEW_OVERDUE_MS, true), State::Permission);
	}

	// ---- session end

	#[test]
	fn late_events_do_not_bring_an_ended_session_back() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![ev("UserPromptSubmit", 10), ev("SessionEnd", 30), tool("PostToolUse", 20, "a", 1)]);
		assert_eq!(live(&t), 0);
		assert!(t.views(31, false).is_empty());
		// a later event is the session being resumed
		feed(&mut t, Tool::Claude, false, vec![ev("UserPromptSubmit", 40)]);
		assert_eq!(state(&t, Tool::Claude, 41, false), State::Working);
		assert_eq!(live(&t), 1);
	}

	#[test]
	fn a_resumed_session_still_knows_its_name_and_desktop() {
		let mut t = tracker(vec![named(Tool::Codex, S, "Fujitu01", Some("D5"))]);
		feed(&mut t, Tool::Codex, false, vec![ev("UserPromptSubmit", 10), ev("SessionEnd", 20)]);
		assert!(t.views(21, false).is_empty());
		// e.g. the app was restarted; no prompt has been typed yet
		feed(&mut t, Tool::Codex, false, vec![ev("SessionStart", 30)]);
		let views = t.views(31, false);
		assert_eq!((views[0].label.as_str(), views[0].desktop.as_deref(), views[0].state), ("Fujitu01", Some("D5"), State::Idle));
	}

	#[test]
	fn an_end_before_anything_else_still_drops_late_events() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![ev("SessionEnd", 30), ev("UserPromptSubmit", 20)]);
		assert_eq!(live(&t), 0);
	}

	#[test]
	fn an_old_session_end_does_not_remove_a_resumed_session() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![ev("UserPromptSubmit", 10), ev("UserPromptSubmit", 40), ev("SessionEnd", 30)]);
		assert_eq!(state(&t, Tool::Claude, 41, false), State::Working);
	}

	#[test]
	fn codex_subagent_events_are_ignored() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Codex, false, vec![ev("UserPromptSubmit", 10)]);
		let sub = Event { agent_type: Some("worker".to_owned()), ..ev("SessionEnd", 20) };
		assert!(!t.apply(Tool::Codex, sub, &[], false));
		assert_eq!(live(&t), 1);
	}

	// ---- desktops

	#[test]
	fn a_window_with_the_sessions_name_places_it() {
		let mut t = tracker(vec![named(Tool::Claude, "a", "GMK01", Some("D5"))]);
		assert!(t.place_by_windows(&[window("GMK01", "D3"), window("Other", "D1")]));
		assert_eq!(t.sessions["Claude:a"].desktop.as_deref(), Some("D3"));
	}

	/// A ChatGPT window showing the Codex thread `thread`.
	fn showing(thread: &str, desktop: &str) -> WindowInfo {
		WindowInfo { process: "ChatGPT".to_owned(), title: thread.to_owned(), desktop: Some(desktop.to_owned()) }
	}

	#[test]
	fn the_chatgpt_window_showing_a_thread_places_it_over_a_terminal() {
		// DoveBar, put on desktop 5 by a terminal that was in front
		let mut t = Tracker::default();
		t.apply(Tool::Codex, ev("UserPromptSubmit", 10), &[terminal("me@server: ~", "D5")], false);
		t.sessions.get_mut(&key(Tool::Codex, S)).unwrap().name = Some("DoveBar".to_owned());
		assert!(t.place_by_names(Tool::Codex, &[showing("DoveBar", "D2"), showing("GMK01", "D3")]));
		assert_eq!(t.sessions[&key(Tool::Codex, S)].desktop.as_deref(), Some("D2"));
		// and only Codex sessions: a Claude session of that name stays
		let mut t = tracker(vec![named(Tool::Claude, "a", "DoveBar", Some("D9"))]);
		assert!(!t.place_by_names(Tool::Codex, &[showing("DoveBar", "D2")]));
		assert_eq!(t.sessions["Claude:a"].desktop.as_deref(), Some("D9"));
	}

	#[test]
	fn same_named_windows_on_different_desktops_place_nothing() {
		let mut t = tracker(vec![named(Tool::Claude, "a", "GMK01", Some("D5")), named(Tool::Claude, "b", "GMK01", None)]);
		assert!(!t.place_by_windows(&[window("GMK01", "D3"), window("GMK01", "D5")]));
		assert_eq!(t.sessions["Claude:a"].desktop.as_deref(), Some("D5"));
		assert_eq!(t.sessions["Claude:b"].desktop, None);
	}

	#[test]
	fn the_one_untitled_window_goes_with_the_one_windowless_session() {
		let mut t = tracker(vec![named(Tool::Claude, "a", "GMK01", Some("D3")), named(Tool::Claude, "b", "Deck", Some("D5"))]);
		assert!(t.place_by_windows(&[window("GMK01", "D3"), window(APP_TITLE, "D1")]));
		assert_eq!(t.sessions["Claude:b"].desktop.as_deref(), Some("D1"));
		// two windowless sessions: no telling which the window is
		let mut t = tracker(vec![named(Tool::Claude, "a", "X", None), named(Tool::Claude, "b", "Y", None)]);
		assert!(!t.place_by_windows(&[window(APP_TITLE, "D1")]));
	}

	#[test]
	fn a_prompt_records_the_desktop_only_when_typed_there_now() {
		let prompt = |ts| ev("UserPromptSubmit", ts);
		let mut t = tracker(vec![named(Tool::Claude, "other", "Other", None)]);
		t.apply(Tool::Claude, prompt(10), &[window("Claude", "D1")], false);
		assert_eq!(t.sessions[&key(Tool::Claude, S)].desktop.as_deref(), Some("D1"));
		// delivered from the queue while the session is busy
		t.apply(Tool::Claude, prompt(20), &[window("Claude", "D2")], false);
		assert_eq!(t.sessions[&key(Tool::Claude, S)].desktop.as_deref(), Some("D1"));
		// another session's window is in front
		t.apply(Tool::Claude, ev("Stop", 30), &[], false);
		t.apply(Tool::Claude, prompt(40), &[window("Other", "D3")], false);
		assert_eq!(t.sessions[&key(Tool::Claude, S)].desktop.as_deref(), Some("D1"));
	}

	fn terminal(title: &str, desktop: &str) -> WindowInfo {
		WindowInfo { process: "WindowsTerminal".to_owned(), title: title.to_owned(), desktop: Some(desktop.to_owned()) }
	}

	#[test]
	fn a_terminal_in_front_does_not_move_a_session_its_app_placed() {
		// DoveBar: placed from the ChatGPT window, then a prompt arrives while
		// an SSH terminal on another desktop is in front
		let mut t = Tracker::default();
		t.apply(Tool::Codex, ev("UserPromptSubmit", 10), &[chatgpt("D2")], false);
		t.apply(Tool::Codex, ev("Stop", 20), &[], false);
		t.apply(Tool::Codex, ev("UserPromptSubmit", 30), &[terminal("me@server: ~", "D5")], false);
		assert_eq!(t.sessions[&key(Tool::Codex, S)].desktop.as_deref(), Some("D2"));
		// its app still moves it
		t.apply(Tool::Codex, ev("Stop", 40), &[], false);
		t.apply(Tool::Codex, ev("UserPromptSubmit", 50), &[chatgpt("D4")], false);
		assert_eq!(t.sessions[&key(Tool::Codex, S)].desktop.as_deref(), Some("D4"));
		// a session only ever seen in a terminal (a CLI) follows the terminal
		let mut t = Tracker::default();
		t.apply(Tool::Codex, ev("UserPromptSubmit", 10), &[terminal("codex", "D3")], false);
		t.apply(Tool::Codex, ev("Stop", 20), &[], false);
		t.apply(Tool::Codex, ev("UserPromptSubmit", 30), &[terminal("codex", "D6")], false);
		assert_eq!(t.sessions[&key(Tool::Codex, S)].desktop.as_deref(), Some("D6"));
	}

	#[test]
	fn codex_prompt_sent_while_busy_is_placed_but_claude_one_is_not() {
		// Codex takes a prompt in at once; Claude Code holds it until it is free
		for (tool, front, expected) in [(Tool::Codex, chatgpt("D2"), Some("D2")), (Tool::Claude, window("Claude", "D2"), None)] {
			let mut t = Tracker::default();
			let busy = self::tool("PreToolUse", 10, "w", 7);
			feed(&mut t, tool, false, vec![busy]);
			t.apply(tool, ev("UserPromptSubmit", 20), &[front], false);
			assert_eq!(t.sessions[&key(tool, S)].desktop.as_deref(), expected, "{tool:?}");
		}
	}

	#[test]
	fn a_prompt_is_placed_by_a_window_it_could_have_been_typed_into() {
		// Codex's hook came after the user had clicked the Claude window:
		// the ChatGPT window that was in front just before is the one.
		let mut t = Tracker::default();
		t.apply(Tool::Codex, ev("UserPromptSubmit", 10), &[window("Fujitu01", "D2"), chatgpt("D5")], false);
		assert_eq!(t.sessions[&key(Tool::Codex, S)].desktop.as_deref(), Some("D5"));
		// nothing it could have been typed into: not recorded
		let firefox = WindowInfo { process: "firefox".to_owned(), title: "x".to_owned(), desktop: Some("D1".to_owned()) };
		let mut t = Tracker::default();
		t.apply(Tool::Codex, ev("UserPromptSubmit", 10), &[firefox, window("Fujitu01", "D2")], false);
		assert_eq!(t.sessions[&key(Tool::Codex, S)].desktop, None);
	}

	#[test]
	fn codex_borrows_the_same_named_claude_desktop() {
		let codex = named(Tool::Codex, "x", "GMK01", None);
		let t = tracker(vec![
			codex.clone(),
			named(Tool::Claude, "a", "gmk01 ", Some("D3")),
			named(Tool::Claude, "b", "GMK02", Some("D4")),
		]);
		assert_eq!(t.namesake_desktop(&codex).as_deref(), Some("D3"));
	}

	#[test]
	fn no_desktop_when_namesakes_disagree_or_are_missing() {
		let codex = named(Tool::Codex, "x", "GMK01", None);
		let split = tracker(vec![named(Tool::Claude, "a", "GMK01", Some("D3")), named(Tool::Claude, "b", "GMK01", Some("D5"))]);
		assert_eq!(split.namesake_desktop(&codex), None);
		let only_codex = tracker(vec![named(Tool::Codex, "y", "GMK01", Some("D3"))]);
		assert_eq!(only_codex.namesake_desktop(&codex), None);
	}

	// ---- refresh key

	fn chatgpt(desktop: &str) -> WindowInfo {
		WindowInfo { process: "ChatGPT".to_owned(), title: "ChatGPT".to_owned(), desktop: Some(desktop.to_owned()) }
	}

	/// A session that has answered and is waiting to be read.
	fn done(tool: Tool, id: &str, name: &str, desktop: Option<&str>) -> Session {
		Session { state: State::Done, since: 1_000, last: 1_000, ..named(tool, id, name, desktop) }
	}

	#[test]
	fn refresh_removes_claude_sessions_whose_window_is_gone() {
		let mut t = tracker(vec![
			done(Tool::Claude, "a", "GMK01", Some("D3")),
			done(Tool::Claude, "b", "Closed", Some("D4")),
			done(Tool::Claude, "c", "Deck", Some("D1")), // its window is the untitled one
		]);
		let claude = [window("GMK01", "D3"), window(APP_TITLE, "D1")];
		assert_eq!(t.sweep(2_000, false, &claude, &[], None), vec!["Claude Closed"]);
		assert_eq!(live(&t), 2);
		// pressing again finds nothing more
		assert!(t.sweep(2_001, false, &claude, &[], None).is_empty());
	}

	#[test]
	fn a_session_cleared_by_mistake_comes_back_with_its_desktop() {
		let mut t = tracker(vec![done(Tool::Codex, S, "Here", Some("D4"))]);
		assert_eq!(t.sweep(2_000, false, &[], &[], None).len(), 1);
		assert!(t.views(2_001, false).is_empty());
		feed(&mut t, Tool::Codex, false, vec![tool("PreToolUse", 3_000, "a", 1)]);
		let views = t.views(3_001, false);
		assert_eq!((views[0].desktop.as_deref(), views[0].state), (Some("D4"), State::Working));
	}

	#[test]
	fn refresh_keeps_sessions_that_need_the_user_or_are_working() {
		let mut t = Tracker::default();
		feed(&mut t, Tool::Claude, false, vec![tool("PreToolUse", 1_000, "a", 1), ask(1_001, 1)]);
		let mut working = named(Tool::Codex, "w", "Busy", Some("D9"));
		working.state = State::Working;
		working.last = 1_500;
		t.sessions.insert(key(Tool::Codex, "w"), working);
		// nothing shows the Codex one either, as far as is known
		assert!(t.sweep(2_000, false, &[], &[], Some(&[])).is_empty());
		// working with no sign of life for long enough is fair game
		assert_eq!(t.sweep(1_500 + STALE_MS + 1, false, &[], &[], Some(&[])).len(), 1);
	}

	#[test]
	fn refresh_removes_codex_threads_no_chatgpt_window_shows() {
		// Desktop 1's ChatGPT window shows the review; "Schedule" there was closed.
		let mut t = tracker(vec![
			done(Tool::Codex, "a", "Review", Some("D1")),
			done(Tool::Codex, "b", "Schedule", Some("D1")),
			done(Tool::Codex, "c", "Elsewhere", Some("D1")), // shown in another desktop's window
		]);
		let threads = [showing("Review", "D1"), showing("Elsewhere", "D3")];
		assert_eq!(t.sweep(2_000, false, &[], &[chatgpt("D1"), chatgpt("D3")], Some(&threads)), vec!["Codex Schedule"]);
		assert_eq!(live(&t), 2);
	}

	#[test]
	fn without_the_threads_refresh_judges_codex_by_its_desktop_or_by_quiet_time() {
		let mut t = tracker(vec![
			done(Tool::Codex, "a", "Here", Some("D3")),
			done(Tool::Codex, "b", "WindowGone", Some("D4")),
			done(Tool::Codex, "c", "NoDesktop", None),
		]);
		assert_eq!(t.sweep(2_000, false, &[], &[chatgpt("D3")], None), vec!["Codex WindowGone"]);
		assert_eq!(t.sweep(1_000 + SWEEP_IDLE_MS + 1, false, &[], &[chatgpt("D3")], None), vec!["Codex NoDesktop"]);
		assert_eq!(live(&t), 1);
	}

	// ---- files

	#[test]
	fn a_title_far_from_the_end_of_the_transcript_is_found() {
		let path = std::env::temp_dir().join(format!("agentj-title-test-{}.jsonl", std::process::id()));
		let filler = format!("{{\"type\":\"user\",\"text\":\"{}\"}}\n", "x".repeat(64 * 1024));
		let mut text = String::from("{\"type\":\"custom-title\",\"customTitle\":\"First\",\"sessionId\":\"s\"}\n");
		for _ in 0..40 {
			text.push_str(&filler); // well over 1 MiB after the title
		}
		std::fs::write(&path, &text).unwrap();
		let path_str = path.to_str().unwrap();

		let (title, offset) = claude_title(path_str, 0);
		assert_eq!(title.as_deref(), Some("First"));
		assert_eq!(offset, text.len() as u64);

		// later calls read only what was added; a half-written line waits
		let more = "{\"type\":\"custom-title\",\"customTitle\":\"Second\"}\n{\"type\":\"custom-title\",\"customTitle\":\"Half";
		std::fs::write(&path, format!("{text}{more}")).unwrap();
		let (title, next) = claude_title(path_str, offset);
		assert_eq!(title.as_deref(), Some("Second"));
		assert_eq!(next, offset + more.find('\n').unwrap() as u64 + 1);
		assert_eq!(claude_title(path_str, next), (None, next));
		let _ = std::fs::remove_file(&path);
	}

	#[test]
	fn codex_config_says_who_reviews() {
		assert!(auto_review_in("model = \"x\"\napprovals_reviewer = \"guardian_subagent\"\n[profiles.a]\n"));
		assert!(auto_review_in("approvals_reviewer = 'auto_review' # mine"));
		assert!(!auto_review_in("approvals_reviewer = \"user\""));
		assert!(!auto_review_in("approval_policy = \"on-request\"\n[profiles.a]\napprovals_reviewer = \"auto_review\""));
	}
}
