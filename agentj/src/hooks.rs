//! Adds or removes the hooks that report Claude Code and Codex session state
//! to the plugin (`agentj.exe --install-hooks` / `--uninstall-hooks`).
//!
//!   Claude Code: `~/.claude/settings.json`
//!   Codex:       `~/.codex/hooks.json` (or under `CODEX_HOME`)
//!
//! Every hook runs `agentj-hook.exe`, which sits next to this exe, in the
//! background ("async"); it exits 0 at once and prints nothing, so the agents
//! never wait on it. Only handlers that run agentj-hook are added or removed:
//! everything else in the file stays, in its order, also other handlers in the
//! same hook group. A file is rewritten only when its content changes, and
//! the previous one is then kept as `<file>.bak-<time>`.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// A handler is ours when its command mentions this.
const MARKER: &str = "agentj-hook";

/// One agent's hook file and what goes in it.
struct Target {
	name: &'static str,
	/// Argument telling agentj-hook which agent calls it.
	arg: &'static str,
	file: PathBuf,
	events: &'static [&'static str],
	/// Events the agent runs synchronously whatever the handler says.
	sync: &'static [&'static str],
	/// What the user still has to do after installing.
	after: &'static str,
}

fn home() -> PathBuf {
	PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default())
}

fn targets() -> Vec<Target> {
	let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex"));
	vec![
		Target {
			name: "Claude Code",
			arg: "claude",
			file: home().join(".claude").join("settings.json"),
			events: &[
				"SessionStart",
				"UserPromptSubmit",
				"PreToolUse",
				"PostToolUse",
				"PostToolUseFailure",
				"PostToolBatch",
				"PermissionRequest",
				"Notification",
				"Stop",
				"StopFailure",
				"SessionEnd",
			],
			sync: &[],
			after: "",
		},
		Target {
			name: "Codex",
			arg: "codex",
			file: codex_home.join("hooks.json"),
			events: &["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "Interrupt", "SessionEnd"],
			sync: &["SessionEnd"], // Codex always runs SessionEnd synchronously
			after: "In the Codex app, approve the hooks under Settings > Hooks; they do not run until then.",
		},
	]
}

/// The command a hook runs: agentj-hook next to this exe. Forward slashes,
/// because the agents hand the command to a shell that may eat backslashes.
fn hook_command(exe_dir: &Path, arg: &str) -> String {
	let exe = exe_dir.join("agentj-hook.exe").to_string_lossy().replace('\\', "/");
	if exe.contains(' ') { format!("\"{exe}\" {arg}") } else { format!("{exe} {arg}") }
}

fn is_ours(handler: &Value) -> bool {
	handler.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(MARKER))
}

/// `settings` without our handlers and, when `command` is given, with them
/// added again for every one of `events`. A group that also holds someone
/// else's handler keeps that handler; a group left empty goes.
fn merged(mut settings: Value, command: Option<&str>, events: &[&str], sync: &[&str]) -> Result<Value, String> {
	// Edited in place, and removed with `shift_remove`, so that the user's
	// keys stay where they are in the file.
	let root = settings.as_object_mut().ok_or("the file is not a JSON object")?;
	if matches!(root.get("hooks"), None | Some(Value::Null)) {
		root.insert("hooks".to_owned(), Value::Object(Map::new()));
	}
	let hooks = root.get_mut("hooks").and_then(Value::as_object_mut).ok_or("\"hooks\" is not an object")?;

	let names: Vec<String> = hooks.keys().cloned().collect();
	for event in names {
		let Some(Value::Array(groups)) = hooks.get_mut(&event) else { continue };
		let had = groups.len();
		groups.retain_mut(|group| {
			let Some(Value::Array(handlers)) = group.get_mut("hooks") else { return true };
			let before = handlers.len();
			handlers.retain(|handler| !is_ours(handler));
			handlers.len() == before || !handlers.is_empty()
		});
		if groups.is_empty() && had > 0 {
			hooks.shift_remove(&event);
		}
	}

	if let Some(command) = command {
		for event in events {
			let handler = if sync.contains(event) {
				json!({ "type": "command", "command": command, "timeout": 2 })
			} else {
				json!({ "type": "command", "command": command, "timeout": 5, "async": true })
			};
			let groups = hooks.entry(*event).or_insert_with(|| Value::Array(Vec::new()));
			match groups {
				Value::Array(groups) => groups.push(json!({ "hooks": [handler] })),
				_ => return Err(format!("\"hooks.{event}\" is not a list")),
			}
		}
	}
	if hooks.is_empty() {
		root.shift_remove("hooks");
	}
	Ok(settings)
}

/// Bring one file up to date. Says what was done.
fn apply(target: &Target, command: Option<&str>) -> Result<String, String> {
	let file = &target.file;
	let before = match std::fs::read_to_string(file) {
		Ok(text) => Some(text),
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
		Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
	};
	if before.is_none() && command.is_none() {
		return Ok(format!("nothing to remove ({} does not exist)", file.display()));
	}
	let text = before.as_deref().unwrap_or("{}").trim_start_matches('\u{feff}');
	let old: Value = if text.trim().is_empty() {
		json!({})
	} else {
		serde_json::from_str(text).map_err(|e| format!("{} is not valid JSON, left alone: {e}", file.display()))?
	};
	let new = merged(old.clone(), command, target.events, target.sync).map_err(|e| format!("{}: {e}, left alone", file.display()))?;
	if new == old && before.is_some() {
		return Ok(format!("already up to date ({})", file.display()));
	}

	if before.is_some() {
		let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
		let backup = PathBuf::from(format!("{}.bak-{stamp}", file.display()));
		std::fs::copy(file, &backup).map_err(|e| format!("cannot back up {}: {e}", file.display()))?;
	}
	if let Some(parent) = file.parent() {
		std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
	}
	let mut json = serde_json::to_string_pretty(&new).map_err(|e| e.to_string())?;
	json.push('\n');
	std::fs::write(file, json).map_err(|e| format!("cannot write {}: {e}", file.display()))?;
	Ok(format!("{} ({})", if command.is_some() { "hooks installed" } else { "hooks removed" }, file.display()))
}

/// `--install-hooks` / `--uninstall-hooks`, for the agents named in `only`
/// ("claude", "codex"), or for both. Prints what it did; false if any failed.
pub fn run(install: bool, only: &[String]) -> bool {
	let exe_dir = crate::paths::exe_dir();
	if install && !exe_dir.join("agentj-hook.exe").exists() {
		println!("agentj-hook.exe is not next to this exe ({}); nothing changed.", exe_dir.display());
		return false;
	}
	let mut ok = true;
	for target in targets().iter().filter(|t| only.is_empty() || only.iter().any(|o| o.eq_ignore_ascii_case(t.arg))) {
		let command = install.then(|| hook_command(&exe_dir, target.arg));
		match apply(target, command.as_deref()) {
			Ok(done) => {
				println!("{}: {done}", target.name);
				if install && !target.after.is_empty() {
					println!("  {}", target.after);
				}
			}
			Err(e) => {
				println!("{}: FAILED: {e}", target.name);
				ok = false;
			}
		}
	}
	ok
}

#[cfg(test)]
mod tests {
	use super::*;

	const CMD: &str = "C:/x/agentj-hook.exe claude";

	fn commands(settings: &Value, event: &str) -> Vec<Vec<String>> {
		settings["hooks"][event]
			.as_array()
			.map(|groups| {
				groups
					.iter()
					.map(|g| g["hooks"].as_array().unwrap().iter().map(|h| h["command"].as_str().unwrap().to_owned()).collect())
					.collect()
			})
			.unwrap_or_default()
	}

	#[test]
	fn installs_into_an_empty_file_and_is_then_up_to_date() {
		let once = merged(json!({}), Some(CMD), &["Stop", "SessionEnd"], &["SessionEnd"]).unwrap();
		assert_eq!(once["hooks"]["Stop"][0]["hooks"][0], json!({ "type": "command", "command": CMD, "timeout": 5, "async": true }));
		assert_eq!(once["hooks"]["SessionEnd"][0]["hooks"][0], json!({ "type": "command", "command": CMD, "timeout": 2 }));
		let twice = merged(once.clone(), Some(CMD), &["Stop", "SessionEnd"], &["SessionEnd"]).unwrap();
		assert_eq!(twice, once);
	}

	#[test]
	fn other_handlers_stay_even_in_the_same_group() {
		let settings = json!({
			"permissions": { "allow": ["Bash(ls)"] },
			"hooks": {
				"PreToolUse": [
					{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "python other.py" }, { "type": "command", "command": "D:/old/agentj-hook.exe claude" }] },
					{ "hooks": [{ "type": "command", "command": "D:/old/agentj-hook.exe claude" }] }
				],
				"SubagentStop": [{ "hooks": [{ "type": "command", "command": "echo keep" }] }]
			}
		});
		let installed = merged(settings.clone(), Some(CMD), &["PreToolUse", "Stop"], &[]).unwrap();
		assert_eq!(commands(&installed, "PreToolUse"), vec![vec!["python other.py".to_owned()], vec![CMD.to_owned()]]);
		assert_eq!(installed["hooks"]["PreToolUse"][0]["matcher"], "Bash");
		assert_eq!(commands(&installed, "SubagentStop"), vec![vec!["echo keep".to_owned()]]);
		assert_eq!(installed["permissions"], settings["permissions"]);

		let removed = merged(installed, None, &["PreToolUse", "Stop"], &[]).unwrap();
		assert_eq!(commands(&removed, "PreToolUse"), vec![vec!["python other.py".to_owned()]]);
		assert!(removed["hooks"].get("Stop").is_none());
	}

	#[test]
	fn removing_the_last_hook_leaves_no_empty_hooks_section() {
		let installed = merged(json!({ "model": "x" }), Some(CMD), &["Stop"], &[]).unwrap();
		assert_eq!(merged(installed, None, &["Stop"], &[]).unwrap(), json!({ "model": "x" }));
	}

	#[test]
	fn keeps_the_order_of_what_is_already_there() {
		let text = r#"{"zeta":1,"hooks":{"Stop":[{"hooks":[{"command":"z"}]}],"Alpha":[{"hooks":[{"command":"a"}]}]},"alpha":2}"#;
		let settings: Value = serde_json::from_str(text).unwrap();
		// nothing of ours in it: taking ours out changes nothing, not even the order
		assert_eq!(serde_json::to_string(&merged(settings.clone(), None, &[], &[]).unwrap()).unwrap(), text);
		// ours goes in after what is there
		let installed = serde_json::to_string(&merged(settings, Some(CMD), &["Stop"], &[]).unwrap()).unwrap();
		assert!(installed.starts_with(r#"{"zeta":1,"hooks":{"Stop":[{"hooks":[{"command":"z"}]},{"hooks":[{"type":"command""#), "{installed}");
		assert!(installed.ends_with(r#""Alpha":[{"hooks":[{"command":"a"}]}]},"alpha":2}"#), "{installed}");
	}

	#[test]
	fn refuses_what_it_does_not_understand() {
		assert!(merged(json!([1, 2]), Some(CMD), &["Stop"], &[]).is_err());
		assert!(merged(json!({ "hooks": 3 }), Some(CMD), &["Stop"], &[]).is_err());
		assert!(merged(json!({ "hooks": { "Stop": "x" } }), Some(CMD), &["Stop"], &[]).is_err());
	}

	#[test]
	fn the_command_is_quoted_only_when_the_path_has_a_space() {
		assert_eq!(hook_command(Path::new(r"C:\a\b"), "codex"), "C:/a/b/agentj-hook.exe codex");
		assert_eq!(hook_command(Path::new(r"C:\Users\A B\x"), "claude"), "\"C:/Users/A B/x/agentj-hook.exe\" claude");
	}
}
