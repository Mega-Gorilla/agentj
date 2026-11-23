//! Hook command for Claude Code and Codex: forwards the hook event to the
//! AgentJ plugin running inside OpenDeck.
//!
//!   agentj-hook.exe claude   (Claude Code hook, event JSON on stdin)
//!   agentj-hook.exe codex    (Codex hook, event JSON on stdin)
//!
//! It keeps only the few fields the plugin needs, adds the time of the hook,
//! and posts it to 127.0.0.1. Tool input is not forwarded, only a hash of it
//! (`call`), which lets the plugin tell which tool call an approval request
//! belongs to. It always exits 0 and prints nothing, so it can never block,
//! slow down or change what the agent does, even when OpenDeck is not running.
#![windows_subsystem = "windows"]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PORT: u16 = 47811;
const KEEP: &[&str] = &[
	"hook_event_name",
	"session_id",
	"cwd",
	"transcript_path",
	"notification_type",
	"source",
	"reason",
	"session_title",
	"agent_id",
	"agent_type",
	"tool_use_id",
];

/// FNV-1a: the same input must hash the same in every hook process.
fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
	bytes.iter().fold(hash, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Hash a JSON value so that the order of an object's keys does not matter.
fn hash_value(hash: u64, value: &serde_json::Value) -> u64 {
	match value {
		serde_json::Value::Object(map) => {
			let mut keys: Vec<&String> = map.keys().collect();
			keys.sort();
			keys.into_iter().fold(fnv1a(hash, b"{"), |h, key| hash_value(fnv1a(fnv1a(h, key.as_bytes()), b":"), &map[key]))
		}
		serde_json::Value::Array(items) => items.iter().fold(fnv1a(hash, b"["), |h, item| fnv1a(hash_value(h, item), b",")),
		other => fnv1a(hash, other.to_string().as_bytes()),
	}
}

/// A hash that is the same for the PreToolUse, PermissionRequest and
/// PostToolUse of one tool call. The command alone when there is one: Codex
/// adds fields to the approval request's input that the others do not have.
fn call_hash(event: &serde_json::Map<String, serde_json::Value>) -> Option<u64> {
	let input = event.get("tool_input").filter(|v| !v.is_null())?;
	let seed = 0xcbf2_9ce4_8422_2325;
	let hash = match input.get("command").filter(|v| !v.is_null()) {
		Some(command) => hash_value(seed, command),
		None => {
			let name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
			hash_value(fnv1a(seed, name.as_bytes()), input)
		}
	};
	Some(hash.max(1)) // 0 means "unknown" to the plugin
}

fn main() {
	// Never hang around, whatever happens with stdin or the socket.
	std::thread::spawn(|| {
		std::thread::sleep(Duration::from_secs(3));
		std::process::exit(0);
	});

	let tool = std::env::args().nth(1).unwrap_or_default();
	let path = if tool.eq_ignore_ascii_case("codex") { "/codex" } else { "/claude" };

	let mut input = Vec::new();
	let _ = std::io::stdin().take(64 << 20).read_to_end(&mut input);
	let Ok(serde_json::Value::Object(event)) = serde_json::from_slice::<serde_json::Value>(&input) else {
		return;
	};

	let mut out = serde_json::Map::new();
	for key in KEEP {
		if let Some(value) = event.get(*key).filter(|v| !v.is_null()) {
			out.insert((*key).to_owned(), value.clone());
		}
	}
	let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
	out.insert("ts".to_owned(), ts.into());
	if let Some(call) = call_hash(&event) {
		out.insert("call".to_owned(), call.into());
	}
	let Ok(body) = serde_json::to_vec(&out) else { return };

	let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
	let Ok(mut socket) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else { return };
	let _ = socket.set_write_timeout(Some(Duration::from_millis(500)));
	let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
	let head = format!(
		"POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
		body.len()
	);
	if socket.write_all(head.as_bytes()).and_then(|_| socket.write_all(&body)).is_ok() {
		let mut reply = [0u8; 64];
		let _ = socket.read(&mut reply);
	}
}

#[cfg(test)]
mod tests {
	use super::call_hash;

	fn hash(json: &str) -> Option<u64> {
		let serde_json::Value::Object(event) = serde_json::from_str(json).unwrap() else { panic!("not an object") };
		call_hash(&event)
	}

	#[test]
	fn one_call_hashes_the_same_in_every_event() {
		let pre = hash(r#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"gh api x"}}"#);
		// Codex adds a description to the approval request, and key order may differ
		let ask = hash(r#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"description":"why","command":"gh api x"}}"#);
		assert!(pre.is_some());
		assert_eq!(pre, ask);
		assert_ne!(pre, hash(r#"{"tool_name":"Bash","tool_input":{"command":"gh api y"}}"#));
	}

	#[test]
	fn tools_without_a_command_hash_name_and_input() {
		let edit = hash(r#"{"tool_name":"Edit","tool_input":{"file_path":"a","old_string":"x","new_string":"y"}}"#);
		assert_eq!(edit, hash(r#"{"tool_name":"Edit","tool_input":{"old_string":"x","new_string":"y","file_path":"a"}}"#));
		assert_ne!(edit, hash(r#"{"tool_name":"Write","tool_input":{"file_path":"a","old_string":"x","new_string":"y"}}"#));
		assert_eq!(hash(r#"{"hook_event_name":"Stop"}"#), None);
	}
}
