//! Demo mode, for screenshots: the keys show made-up desktops and sessions
//! instead of the real ones, whose names may be private. Each part draws its
//! own made-up content (`desktops::keys`, `agents::keys`); everything else,
//! switching desktops and taking hook events, keeps working as usual.
//!
//! On while a file named `demo` is in the data folder (`paths`), so it can be
//! switched without a restart:
//!
//!   New-Item "$env:LOCALAPPDATA\AgentJ\demo"      # on
//!   Remove-Item "$env:LOCALAPPDATA\AgentJ\demo"   # off

pub fn on() -> bool {
	crate::paths::data_file("demo").exists()
}
