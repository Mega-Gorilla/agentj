//! Key images, drawn as 144x144 SVG and sent as data URLs.

use base64::Engine;

use crate::agents::{State, Tool};

const FONT: &str = "'Yu Gothic UI','Meiryo','Segoe UI',sans-serif";
/// Usable text width inside the key.
const TEXT_W: f32 = 128.0;

pub fn data_url(svg: &str) -> String {
	format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg))
}

fn svg(background: &str, inner: &str) -> String {
	format!(
		r#"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144" font-family="{FONT}"><rect width="144" height="144" fill="{background}"/>{inner}</svg>"#
	)
}

fn escape(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
		.replace('\'', "&apos;")
}

/// Approximate rendered width of `s` in em (CJK is about 1em, Latin about half).
fn em_width(s: &str) -> f32 {
	s.chars()
		.map(|c| match c {
			' ' => 0.3,
			c if c.is_ascii_uppercase() || c.is_ascii_digit() => 0.64,
			c if c.is_ascii() => 0.54,
			_ => 1.0,
		})
		.sum()
}

/// Pick the largest font size (up to `max`) that fits; below `min`, cut the
/// text and add an ellipsis.
fn fit(s: &str, max: f32, min: f32) -> (String, f32) {
	let width = em_width(s).max(0.1);
	let size = (TEXT_W / width).min(max);
	if size >= min {
		return (s.to_owned(), size.floor());
	}
	let mut out = String::new();
	for c in s.chars() {
		let candidate = format!("{out}{c}…");
		if em_width(&candidate) * min > TEXT_W {
			break;
		}
		out.push(c);
	}
	(format!("{out}…"), min)
}

fn centered(y: f32, size: f32, fill: &str, weight: u16, text: &str) -> String {
	format!(
		r#"<text x="72" y="{y}" font-size="{size}" font-weight="{weight}" fill="{fill}" text-anchor="middle" dominant-baseline="central">{}</text>"#,
		escape(text)
	)
}

/// A desktop key. `name` is empty when the desktop was never renamed.
pub fn slot(index: usize, name: &str, current: bool) -> String {
	let (background, fg, sub) = if current { ("#2563EB", "#FFFFFF", "#DBEAFE") } else { ("#1E293B", "#E2E8F0", "#94A3B8") };
	let number = (index + 1).to_string();
	let mut inner = if name.trim().is_empty() {
		centered(70.0, 64.0, fg, 700, &number)
	} else {
		let (text, size) = fit(name.trim(), 40.0, 17.0);
		format!(
			r#"<text x="14" y="36" font-size="26" font-weight="700" fill="{sub}">{number}</text>{}"#,
			centered(88.0, size, fg, 700, &text)
		)
	};
	if current {
		// Not only colour: a bar marks the current desktop too.
		inner.push_str(r##"<rect x="0" y="132" width="144" height="12" fill="#FFFFFF"/>"##);
	}
	svg(background, &inner)
}

/// A slot with no desktop behind it.
pub fn empty() -> String {
	svg("#000000", "")
}

/// "current / total" and the current desktop's name.
pub fn info(current: Option<(usize, &str)>, count: usize) -> String {
	let Some((index, name)) = current else {
		return svg("#0F172A", &centered(72.0, 40.0, "#64748B", 700, "…"));
	};
	let label = if name.trim().is_empty() { format!("デスクトップ {}", index + 1) } else { name.trim().to_owned() };
	let (text, size) = fit(&label, 24.0, 15.0);
	let inner = format!(
		"{}{}",
		centered(52.0, 40.0, "#FFFFFF", 700, &format!("{}/{}", index + 1, count)),
		centered(106.0, size, "#94A3B8", 400, &text)
	);
	svg("#0F172A", &inner)
}

fn elapsed(ms: u64) -> String {
	let minutes = ms / 60_000;
	match minutes {
		0 => "今".to_owned(),
		1..=59 => format!("{minutes}分"),
		60..=1439 => format!("{}時間", minutes / 60),
		_ => format!("{}日", minutes / 1440),
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
		("#27272A", "応答なし?", "#A1A1AA")
	} else {
		match card.state {
			State::Permission => ("#C2410C", "承認待ち", "#FFFFFF"),
			State::Input => ("#B45309", "入力待ち", "#FFFFFF"),
			State::Error => ("#991B1B", "エラー", "#FFFFFF"),
			// Green = an answer is waiting to be read. Working needs nobody,
			// so it stays dark.
			State::Done => ("#15803D", "返答済み", "#DCFCE7"),
			State::Working => ("#000000", "作業中", "#94A3B8"),
			State::Reviewing => ("#1A1A1A", "自動審査中", "#A1A1AA"),
			// Green like Done: a session that is open and free to take a prompt.
			// (A resumed session is Idle even if its last answer is unread.)
			State::Idle => ("#15803D", "待機中", "#DCFCE7"),
		}
	};
	let (tool, mut tool_colour) = match card.tool {
		Tool::Claude => ("Claude", "#F4A98A"),
		Tool::Codex => ("Codex", "#9FE7C9"),
	};
	if !card.stale && !matches!(card.state, State::Working | State::Reviewing) {
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
pub fn agents_summary(attention: usize, done: usize, working: usize) -> String {
	let background = if attention > 0 { "#7C2D12" } else { "#0F172A" };
	let rows = [("要対応", attention, "#FB923C"), ("返答済", done, "#4ADE80"), ("作業中", working, "#94A3B8")];
	let inner: String = rows
		.iter()
		.enumerate()
		.map(|(i, (label, count, colour))| {
			let y = 36.0 + i as f32 * 38.0;
			format!(
				r##"<circle cx="20" cy="{y}" r="8" fill="{colour}"/><text x="36" y="{y}" font-size="22" fill="#E2E8F0" dominant-baseline="central">{label}</text><text x="134" y="{y}" font-size="28" font-weight="700" fill="#FFFFFF" text-anchor="end" dominant-baseline="central">{count}</text>"##
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
		("#16A34A", "#16A34A", r##"<path d="M58 70 L68 80 L87 59" fill="none" stroke="#FFFFFF" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/>"##, "移動する", "#4ADE80")
	} else {
		("none", "#94A3B8", "", "移動しない", "#94A3B8")
	};
	let inner = format!(
		r##"{}<rect x="50" y="48" width="44" height="44" rx="8" fill="{fill}" stroke="{stroke}" stroke-width="4"/>{mark}{}"##,
		centered(26.0, 24.0, name_colour, 700, name),
		centered(118.0, 20.0, label_colour, 700, label)
	);
	svg("#1E293B", &inner)
}

/// The summary key while its settings screen is open: press to close.
pub fn settings_close() -> String {
	let inner = format!(
		"{}{}{}",
		centered(34.0, 22.0, "#94A3B8", 700, "集計キーの"),
		centered(66.0, 30.0, "#FFFFFF", 700, "移動先"),
		centered(112.0, 20.0, "#FBBF24", 700, "押して閉じる")
	);
	svg("#334155", &inner)
}

/// The refresh key. `removed` is how many closed sessions the press just
/// cleared, shown for a moment afterwards.
pub fn refresh(removed: Option<usize>) -> String {
	let (label, colour) = match removed {
		None => ("更新".to_owned(), "#94A3B8"),
		Some(0) => ("消去なし".to_owned(), "#E2E8F0"),
		Some(n) => (format!("{n}件 消去"), "#4ADE80"),
	};
	let inner = format!("{}{}", centered(56.0, 60.0, "#E2E8F0", 400, "↻"), centered(112.0, 22.0, colour, 700, &label));
	svg("#1E293B", &inner)
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

/// Previous / next key. `target` is where it would go, if anywhere.
pub fn arrow(next: bool, target: Option<(usize, &str)>) -> String {
	let glyph = if next { "▶" } else { "◀" };
	match target {
		Some((index, name)) => {
			let label = if name.trim().is_empty() { (index + 1).to_string() } else { name.trim().to_owned() };
			let (text, size) = fit(&label, 22.0, 14.0);
			let inner = format!("{}{}", centered(58.0, 50.0, "#E2E8F0", 400, glyph), centered(112.0, size, "#94A3B8", 400, &text));
			svg("#1E293B", &inner)
		}
		None => svg("#0B1120", &centered(58.0, 50.0, "#334155", 400, glyph)),
	}
}
