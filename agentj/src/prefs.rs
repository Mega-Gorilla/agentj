//! What the user has set on the deck itself. Kept in `prefs.json` in the data
//! folder (`paths`), so it survives restarts, updates and re-applied layouts.

use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::agents::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
	/// The summary key goes to Claude sessions.
	pub jump_claude: bool,
	/// The summary key goes to Codex sessions.
	pub jump_codex: bool,
}

impl Default for Prefs {
	fn default() -> Self {
		Prefs { jump_claude: true, jump_codex: true }
	}
}

impl Prefs {
	/// Whether the summary key goes to this tool's sessions.
	pub fn jumps_to(&self, tool: Tool) -> bool {
		match tool {
			Tool::Claude => self.jump_claude,
			Tool::Codex => self.jump_codex,
		}
	}

	fn toggle_jump(&mut self, tool: Tool) {
		match tool {
			Tool::Claude => self.jump_claude = !self.jump_claude,
			Tool::Codex => self.jump_codex = !self.jump_codex,
		}
	}
}

static PREFS: LazyLock<Mutex<Prefs>> = LazyLock::new(|| Mutex::new(load()));

fn path() -> Option<PathBuf> {
	Some(crate::paths::data_file("prefs.json"))
}

fn load() -> Prefs {
	path().and_then(|p| std::fs::read(p).ok()).and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

pub fn get() -> Prefs {
	*PREFS.lock().unwrap()
}

/// Flip whether the summary key goes to this tool's sessions, and save.
pub fn toggle_jump(tool: Tool) -> Prefs {
	let mut prefs = PREFS.lock().unwrap();
	prefs.toggle_jump(tool);
	if let (Some(path), Ok(json)) = (path(), serde_json::to_vec_pretty(&*prefs)) {
		let _ = std::fs::write(path, json);
	}
	*prefs
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn jumps_everywhere_unless_told_otherwise() {
		let mut prefs: Prefs = serde_json::from_str("{}").unwrap();
		assert!(prefs.jumps_to(Tool::Claude) && prefs.jumps_to(Tool::Codex));
		prefs.toggle_jump(Tool::Codex);
		assert!(prefs.jumps_to(Tool::Claude) && !prefs.jumps_to(Tool::Codex));
		let saved = serde_json::to_string(&prefs).unwrap();
		assert_eq!(serde_json::from_str::<Prefs>(&saved).unwrap(), prefs);
	}
}
