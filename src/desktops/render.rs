//! What the desktop keys look like.

use crate::render::{centered, fit, svg};

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
