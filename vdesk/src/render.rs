//! Key images, drawn as 144x144 SVG and sent as data URLs.

use base64::Engine;

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
