//! Hook command for Claude Code and Codex: forwards the hook event to the
//! vdesk plugin running inside OpenDeck.
//!
//!   agent-hook.exe claude   (Claude Code hook, event JSON on stdin)
//!   agent-hook.exe codex    (Codex hook, event JSON on stdin)
//!
//! It keeps only the few fields the plugin needs, adds the time of the hook,
//! and posts it to 127.0.0.1. It always exits 0 and prints nothing, so it can
//! never block, slow down or change what the agent does, even when OpenDeck is
//! not running.
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
];

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
