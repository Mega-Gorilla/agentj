//! AgentJ, an OpenDeck plugin: AI agent sessions on the keys, a press away,
//! and Windows 11 virtual desktops.
//!
//! Two parts, one plugin:
//! - `desktops`: the virtual desktops and the keys that show and switch them.
//!   It knows nothing of AI agents.
//! - `agents`: Claude Code / Codex sessions, fed by their hooks, and the keys
//!   that show them and jump to their desktop. It uses the desktop part only
//!   through `desktops::get()`.
//!
//! `deck` holds what every key needs and redraws both parts; `render` the
//! drawing helpers; `paths` where data files live. This file only starts
//! things.
//!
//! `agentj.exe --selftest` prints what it reads from Windows and writes
//! preview.html, without switching anything.
//! `agentj.exe --install-hooks` / `--uninstall-hooks` adds or removes the
//! Claude Code and Codex hooks (see agents/hooks.rs).

mod agents;
mod deck;
mod desktops;
mod paths;
mod render;

use std::time::Duration;

fn init_logging(to_file: bool) {
	use simplelog::*;
	let config = ConfigBuilder::new().set_time_offset_to_local().unwrap_or_else(|b| b).build();
	// Keep the previous run's log: a restart (deploy) would otherwise wipe
	// what led up to it.
	let file = to_file
		.then(|| paths::data_file("agentj.log"))
		.and_then(|p| {
			let _ = std::fs::rename(&p, p.with_file_name("agentj.prev.log"));
			std::fs::File::create(p).ok()
		});
	let _ = match file {
		Some(f) => WriteLogger::init(LevelFilter::Info, config, f),
		None => TermLogger::init(LevelFilter::Info, config, TerminalMode::Stdout, ColorChoice::Never),
	};
}

fn preview_grid(title: &str, cells: &[String]) -> String {
	let imgs: String = cells.iter().map(|s| format!(r#"<img src="{}">"#, render::data_url(s))).collect();
	format!(r#"<h2>{title}</h2><div>{imgs}</div>"#)
}

/// Read-only check: what winvd sees, a no-op switch to the current desktop,
/// and a preview of both key layouts (the AI one with made-up sessions).
fn selftest() -> std::process::ExitCode {
	let snap = match desktops::read() {
		Ok(s) => s,
		Err(e) => {
			println!("FAILED to read desktops: {e:?}");
			return std::process::ExitCode::FAILURE;
		}
	};
	println!("desktops: {}", snap.count());
	for (i, name) in snap.names.iter().enumerate() {
		println!("  {}{}: {:?}", i + 1, if i == snap.current { " *" } else { "" }, name);
	}
	match winvd::switch_desktop(snap.current as u32) {
		Ok(()) => println!("switch call (to the current desktop, no visible change): ok"),
		Err(e) => println!("switch call FAILED: {e:?} (keyboard fallback would be used)"),
	}
	let desktop_of = |w: &agents::winutil::WindowInfo| w.desktop.as_ref().and_then(|g| snap.ids.iter().position(|x| x == g)).map(|i| i + 1);
	println!("claude windows:");
	for w in agents::winutil::app_windows(&["claude"]) {
		println!("  desktop {:?}: {:?}", desktop_of(&w), w.title);
	}
	println!("codex threads shown in ChatGPT windows:");
	for w in agents::winutil::chatgpt_threads() {
		println!("  desktop {:?}: {:?}", desktop_of(&w), w.title);
	}

	let grids: String = agents::keys::preview().iter().map(|(title, keys)| preview_grid(title, keys)).collect();
	let html = format!(
		r#"<!doctype html><meta charset="utf-8"><title>AgentJ preview</title><style>body{{background:#111;color:#ccc;font-family:sans-serif;margin:24px}}div{{display:grid;grid-template-columns:repeat(5,96px);gap:12px;margin-bottom:24px}}img{{width:96px;height:96px;border-radius:12px}}h2{{font-size:14px}}</style>{}{grids}"#,
		preview_grid("Default", &desktops::keys::preview(&snap))
	);
	let path = std::env::current_dir().unwrap_or_default().join("preview.html");
	match std::fs::write(&path, html) {
		Ok(()) => println!("preview: {}", path.display()),
		Err(e) => println!("could not write preview: {e}"),
	}
	std::process::ExitCode::SUCCESS
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
	let args: Vec<String> = std::env::args().collect();
	if args.iter().any(|a| a == "--selftest") {
		init_logging(false);
		return selftest();
	}
	// `--install-hooks [claude] [codex]` / `--uninstall-hooks [claude] [codex]`
	if let Some(at) = args.iter().position(|a| a == "--install-hooks" || a == "--uninstall-hooks") {
		let ok = agents::hooks::run(args[at] == "--install-hooks", &args[at + 1..]);
		return if ok { std::process::ExitCode::SUCCESS } else { std::process::ExitCode::FAILURE };
	}

	init_logging(true);
	log::info!("agentj {} starting", env!("CARGO_PKG_VERSION"));
	desktops::init();
	agents::front::start();

	desktops::keys::register().await;
	agents::keys::register().await;

	tokio::spawn(agents::serve());
	tokio::spawn(async {
		let mut changes = desktops::get().subscribe();
		while changes.changed().await.is_ok() {
			deck::redraw_all().await;
		}
	});
	tokio::spawn(async {
		let mut changes = agents::subscribe();
		while changes.changed().await.is_ok() {
			deck::redraw_all().await;
		}
	});
	// Names, window positions, "minutes ago" and stale sessions.
	tokio::spawn(async {
		let mut tick = tokio::time::interval(Duration::from_secs(3));
		loop {
			tick.tick().await;
			agents::maintain().await;
			deck::redraw_all().await;
		}
	});

	match openaction::run(args).await {
		Ok(()) => std::process::ExitCode::SUCCESS,
		Err(e) => {
			log::error!("plugin stopped: {e}");
			std::process::ExitCode::FAILURE
		}
	}
}
