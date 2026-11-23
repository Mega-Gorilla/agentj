//! Where AgentJ keeps what it must not lose: its session list, settings and
//! log. Not next to the exe, because OpenDeck replaces the whole plugin folder
//! when the plugin is updated.

use std::path::PathBuf;

/// `%LOCALAPPDATA%\AgentJ`, created if need be. Falls back to the exe's
/// folder when that cannot be had.
pub fn data_dir() -> PathBuf {
	let dir = std::env::var_os("LOCALAPPDATA").map(|base| PathBuf::from(base).join("AgentJ"));
	match dir {
		Some(dir) if std::fs::create_dir_all(&dir).is_ok() => dir,
		_ => exe_dir(),
	}
}

/// The folder the running exe is in: the plugin folder.
pub fn exe_dir() -> PathBuf {
	std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)).unwrap_or_default()
}

/// The path of a data file.
pub fn data_file(name: &str) -> PathBuf {
	data_dir().join(name)
}
