//! What the AI keys look like.

use super::{State, Tool};
use crate::render::{centered, fit, svg};

fn elapsed(ms: u64) -> String {
	let minutes = ms / 60_000;
	match minutes {
		0 => "now".to_owned(),
		1..=59 => format!("{minutes}m"),
		60..=1439 => format!("{}h", minutes / 60),
		_ => format!("{}d", minutes / 1440),
	}
}

pub struct AgentCard<'a> {
	pub tool: Tool,
	pub name: &'a str,
	pub state: State,
	pub stale: bool,
	/// 0-based desktop index
	pub desktop: usize,
	/// time in the current state
	pub elapsed_ms: u64,
	/// the session is on the desktop being shown
	pub here: bool,
}

/// One AI session: tool, desktop number, name, state and how long it has been in it.
pub fn agent(card: &AgentCard) -> String {
	let (background, label, label_colour) = if card.stale {
		("#27272A", "Stalled?", "#A1A1AA")
	} else {
		match card.state {
			State::Permission => ("#C2410C", "Approve", "#FFFFFF"),
			State::Input => ("#B45309", "Input", "#FFFFFF"),
			State::Error => ("#991B1B", "Error", "#FFFFFF"),
			// Green = an answer is waiting to be read. Working needs nobody,
			// so it stays dark.
			State::Done => ("#15803D", "Answered", "#DCFCE7"),
			State::Working => ("#000000", "Working", "#94A3B8"),
			// The turn is over but work it started goes on: dark like
			// Working, with a blue tint to tell the two apart.
			State::Background => ("#0B1A2E", "Background", "#7DD3FC"),
			State::Reviewing => ("#1A1A1A", "Reviewing", "#A1A1AA"),
			// Green like Done: a session that is open and free to take a prompt.
			// (A resumed session is Idle even if its last answer is unread.)
			State::Idle => ("#15803D", "Idle", "#DCFCE7"),
		}
	};
	let (tool, mut tool_colour) = match card.tool {
		Tool::Claude => ("Claude", "#F4A98A"),
		Tool::Codex => ("Codex", "#9FE7C9"),
	};
	if !card.stale && !matches!(card.state, State::Working | State::Background | State::Reviewing) {
		// The tool colours do not read well on the strong state colours.
		tool_colour = "#FFFFFF";
	}
	let desktop = format!(
		r##"<rect x="100" y="12" width="32" height="26" rx="5" fill="none" stroke="#FFFFFF" stroke-opacity="0.7" stroke-width="2"/><text x="116" y="25" font-size="18" font-weight="700" fill="#FFFFFF" fill-opacity="0.7" text-anchor="middle" dominant-baseline="central">{}</text>"##,
		card.desktop + 1
	);
	let (name, name_size) = fit(card.name, 30.0, 15.0);
	let (status, status_size) = fit(&format!("{label} {}", elapsed(card.elapsed_ms)), 22.0, 14.0);
	// "You are here": a red frame, with a thin white line inside so that it
	// also reads on the orange and red backgrounds.
	let frame = if card.here {
		r##"<rect x="3.5" y="3.5" width="137" height="137" rx="10" fill="none" stroke="#FF3B30" stroke-width="7"/><rect x="7.75" y="7.75" width="128.5" height="128.5" rx="7" fill="none" stroke="#FFFFFF" stroke-width="1.5"/>"##
	} else {
		""
	};
	let inner = format!(
		r#"<text x="14" y="30" font-size="19" font-weight="700" fill="{tool_colour}">{tool}</text>{desktop}{}{}{frame}"#,
		centered(74.0, name_size, "#FFFFFF", 700, &name),
		centered(116.0, status_size, label_colour, 700, &status)
	);
	svg(background, &inner)
}

/// Counts across all sessions; turns orange when something needs the user.
pub fn summary(attention: usize, done: usize, working: usize) -> String {
	let background = if attention > 0 { "#7C2D12" } else { "#0F172A" };
	let rows = [("Needs you", attention, "#FB923C"), ("Answered", done, "#4ADE80"), ("Working", working, "#94A3B8")];
	let inner: String = rows
		.iter()
		.enumerate()
		.map(|(i, (label, count, colour))| {
			let y = 36.0 + i as f32 * 38.0;
			format!(
				r##"<circle cx="18" cy="{y}" r="7" fill="{colour}"/><text x="31" y="{y}" font-size="17" fill="#E2E8F0" dominant-baseline="central">{label}</text><text x="136" y="{y}" font-size="26" font-weight="700" fill="#FFFFFF" text-anchor="end" dominant-baseline="central">{count}</text>"##
			)
		})
		.collect();
	svg(background, &inner)
}

/// A checkbox in the summary key's settings screen: whether that key goes to
/// this tool's sessions.
pub fn jump_option(tool: Tool, on: bool) -> String {
	let (name, name_colour) = match tool {
		Tool::Claude => ("Claude", "#F4A98A"),
		Tool::Codex => ("Codex", "#9FE7C9"),
	};
	let (fill, stroke, mark, label, label_colour) = if on {
		("#16A34A", "#16A34A", r##"<path d="M58 70 L68 80 L87 59" fill="none" stroke="#FFFFFF" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>"##, "Jump: on", "#4ADE80")
	} else {
		("none", "#94A3B8", "", "Jump: off", "#94A3B8")
	};
	let inner = format!(
		r##"{}<rect x="50" y="48" width="44" height="44" rx="8" fill="{fill}" stroke="{stroke}" stroke-width="4"/>{mark}{}"##,
		centered(26.0, 24.0, name_colour, 700, name),
		centered(118.0, 20.0, label_colour, 700, label)
	);
	svg("#1E293B", &inner)
}

/// The refresh key. `removed` is how many closed sessions the press just
/// cleared, shown for a moment afterwards.
pub fn refresh(removed: Option<usize>) -> String {
	let (label, colour) = match removed {
		None => ("Refresh".to_owned(), "#94A3B8"),
		Some(0) => ("None closed".to_owned(), "#E2E8F0"),
		Some(n) => (format!("{n} cleared"), "#4ADE80"),
	};
	let (label, size) = fit(&label, 22.0, 14.0);
	let inner = format!("{}{}", centered(56.0, 60.0, "#E2E8F0", 400, "↻"), centered(112.0, size, colour, 700, &label));
	svg("#1E293B", &inner)
}

// ---- a session key's detail screen

/// One fact about the session: a small title above its value, and an
/// optional second line below.
pub fn info(title: &str, value: &str, more: Option<&str>) -> String {
	let (value, value_size) = fit(value, 28.0, 13.0);
	let value_y = if more.is_some() { 76.0 } else { 84.0 };
	let more = more.map_or_else(String::new, |m| {
		let (m, size) = fit(m, 18.0, 12.0);
		centered(112.0, size, "#CBD5E1", 400, &m)
	});
	let inner = format!(
		"{}{}{more}",
		centered(30.0, 17.0, "#94A3B8", 700, title),
		centered(value_y, value_size, "#FFFFFF", 700, &value)
	);
	svg("#111827", &inner)
}

/// How long ago, for the detail screen.
pub fn ago(ms: u64) -> String {
	match elapsed(ms).as_str() {
		"now" => "just now".to_owned(),
		e => format!("{e} ago"),
	}
}

/// A button: an icon drawn around (72, 58) and a short caption below it.
fn button(icon: &str, caption: &str, background: &str) -> String {
	let (caption, size) = fit(caption, 20.0, 12.0);
	svg(background, &format!("{icon}{}", centered(118.0, size, "#E2E8F0", 700, &caption)))
}

/// Go to the session's desktop: a screen with an arrow into it.
pub fn jump_button(desktop: usize) -> String {
	let icon = r##"<rect x="32" y="26" width="80" height="54" rx="7" fill="none" stroke="#FFFFFF" stroke-width="6"/><rect x="62" y="84" width="20" height="7" fill="#FFFFFF"/><path d="M50 53 H90 M77 40 L90 53 L77 66" fill="none" stroke="#FFFFFF" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>"##;
	button(icon, &format!("Desktop {}", desktop + 1), "#1D4ED8")
}

/// Take the session off the deck: a crossed-out eye.
pub fn hide_button() -> String {
	let background = "#7F1D1D";
	let icon = format!(
		r##"<path d="M28 56 Q72 16 116 56 Q72 96 28 56 Z" fill="none" stroke="#FFFFFF" stroke-width="6" stroke-linejoin="round"/><circle cx="72" cy="56" r="13" fill="#FFFFFF"/><path d="M40 90 L104 22" stroke="{background}" stroke-width="16" stroke-linecap="round"/><path d="M40 90 L104 22" stroke="#FFFFFF" stroke-width="6" stroke-linecap="round"/>"##
	);
	button(&icon, "Hide", background)
}

/// Close a screen drawn over the keys: a cross.
pub fn close_button(caption: &str) -> String {
	let icon = r##"<path d="M48 32 L96 80 M96 32 L48 80" stroke="#FFFFFF" stroke-width="10" stroke-linecap="round"/>"##;
	button(icon, caption, "#334155")
}

/// Previous / next page of AI sessions. `target` is (page, pages) it would go
/// to, if anywhere; orange when a session over there needs the user.
pub fn page_arrow(next: bool, target: Option<(usize, usize)>, attention: bool) -> String {
	let glyph = if next { "▶" } else { "◀" };
	match target {
		Some((page, pages)) => {
			let (background, sub) = if attention { ("#7C2D12", "#FDBA74") } else { ("#1E293B", "#94A3B8") };
			let inner = format!(
				"{}{}",
				centered(58.0, 50.0, "#E2E8F0", 400, glyph),
				centered(112.0, 24.0, sub, 700, &format!("{}/{}", page + 1, pages))
			);
			svg(background, &inner)
		}
		None => svg("#0B1120", &centered(58.0, 50.0, "#334155", 400, glyph)),
	}
}
