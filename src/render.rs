//! Drawing helpers shared by every key: 144x144 SVG, sent as data URLs.
//! What each key shows is drawn in `desktops::render` and `agents::render`.

use base64::Engine;

const FONT: &str = "'Yu Gothic UI','Meiryo','Segoe UI',sans-serif";
/// Usable text width inside the key.
const TEXT_W: f32 = 128.0;

pub fn data_url(svg: &str) -> String {
	format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg))
}

/// A whole key: the background colour and what goes on it.
pub fn svg(background: &str, inner: &str) -> String {
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
pub fn fit(s: &str, max: f32, min: f32) -> (String, f32) {
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

/// A line of text centred across the key, `y` being its middle.
pub fn centered(y: f32, size: f32, fill: &str, weight: u16, text: &str) -> String {
	format!(
		r#"<text x="72" y="{y}" font-size="{size}" font-weight="{weight}" fill="{fill}" text-anchor="middle" dominant-baseline="central">{}</text>"#,
		escape(text)
	)
}

/// A key with nothing behind it.
pub fn empty() -> String {
	svg("#000000", "")
}
