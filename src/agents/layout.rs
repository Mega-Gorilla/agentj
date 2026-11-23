//! Where sessions go on the keys and where the summary key takes the user.
//! Rules only, no keys or I/O, so they are unit tested.

use super::prefs::Prefs;
use super::{State, Tool, View};

/// Where every session goes on one device, page by page.
#[derive(Debug, PartialEq)]
pub struct Pages {
	/// Per page, the session each key shows (an index into the session list).
	pub keys: Vec<Vec<Option<usize>>>,
	/// The page each session is on; None when no key can show it.
	pub page_of: Vec<Option<usize>>,
}

/// Lay the sessions out on one device's AI Session keys.
///
/// `lanes` is the tool of each key, in reading order; `sessions` is (tool,
/// desktop index) of each session, in display order (by desktop). The k-th
/// key of every tool's row makes column k, the Claude row above the Codex
/// row. Each desktop with sessions takes the next column(s): its Claude
/// sessions in the Claude row and its Codex sessions in the Codex row, so one
/// desktop's sessions sit one above the other. Columns run on to further
/// pages, without splitting a desktop across two pages when it fits on one.
/// Keys set to no tool list, in order, the sessions of tools that have no row
/// (all of them when no key is set to a tool).
pub fn paginate(lanes: &[Option<Tool>], sessions: &[(Tool, usize)]) -> Pages {
	const TOOLS: [Tool; 2] = [Tool::Claude, Tool::Codex];
	let rows: Vec<Vec<usize>> =
		TOOLS.iter().map(|t| (0..lanes.len()).filter(|&k| lanes[k] == Some(*t)).collect()).collect();
	let spare: Vec<usize> = (0..lanes.len()).filter(|&k| lanes[k].is_none()).collect();
	let width = rows.iter().map(Vec::len).filter(|&n| n > 0).min().unwrap_or(0);
	let row_of = |j: usize| TOOLS.iter().position(|t| *t == sessions[j].0).filter(|&r| !rows[r].is_empty());

	let mut cells: Vec<(usize, usize, usize)> = Vec::new(); // (row, column, session)
	let mut listed: Vec<usize> = Vec::new();
	let mut column = 0;
	let mut start = 0;
	while start < sessions.len() {
		// One desktop's sessions.
		let desktop = sessions[start].1;
		let end = (start..sessions.len()).find(|&j| sessions[j].1 != desktop).unwrap_or(sessions.len());
		let need = (0..TOOLS.len()).map(|r| (start..end).filter(|&j| row_of(j) == Some(r)).count()).max().unwrap_or(0);
		if need > 0 && need <= width && column % width + need > width {
			column += width - column % width; // start this desktop on the next page
		}
		let mut used = [0usize; TOOLS.len()];
		for j in start..end {
			match row_of(j) {
				Some(r) => {
					cells.push((r, column + used[r], j));
					used[r] += 1;
				}
				None => listed.push(j),
			}
		}
		column += need;
		start = end;
	}

	let grid_pages = if width == 0 { 0 } else { column.div_ceil(width) };
	let list_pages = if spare.is_empty() { 0 } else { listed.len().div_ceil(spare.len()) };
	let count = grid_pages.max(list_pages).max(1);
	let mut keys = vec![vec![None; lanes.len()]; count];
	let mut page_of = vec![None; sessions.len()];
	for (r, c, j) in cells {
		keys[c / width][rows[r][c % width]] = Some(j);
		page_of[j] = Some(c / width);
	}
	if !spare.is_empty() {
		for (n, j) in listed.into_iter().enumerate() {
			keys[n / spare.len()][spare[n % spare.len()]] = Some(j);
			page_of[j] = Some(n / spare.len());
		}
	}
	Pages { keys, page_of }
}

/// Which of one device's AI Session keys (their tools, in reading order) hold
/// the Claude and the Codex checkbox of the summary key's settings screen:
/// the first key of that tool's row, or failing that the next key set to no
/// tool.
pub fn option_keys(lanes: &[Option<Tool>]) -> [(Tool, Option<usize>); 2] {
	let mut spare = (0..lanes.len()).filter(|&k| lanes[k].is_none());
	[Tool::Claude, Tool::Codex].map(|tool| (tool, lanes.iter().position(|l| *l == Some(tool)).or_else(|| spare.next())))
}

/// The desktops with a session waiting for the user, in the order the summary
/// key visits them: approval first, then input, error, answered; within one
/// kind, the longest wait first. Each desktop once. Only sessions of the tools
/// ticked in the key's settings count.
pub fn tour_stops(sessions: &[(View, usize)], prefs: Prefs) -> Vec<usize> {
	let mut waiting: Vec<(u8, u64, usize)> = sessions
		.iter()
		.filter(|(s, _)| prefs.jumps_to(s.tool) && s.state.urgency() <= State::Done.urgency())
		.map(|(s, desktop)| (s.state.urgency(), s.since, *desktop))
		.collect();
	waiting.sort_unstable();
	let mut stops = Vec::new();
	for (_, _, desktop) in waiting {
		if !stops.contains(&desktop) {
			stops.push(desktop);
		}
	}
	stops
}

/// Which of `stops` (not empty) to go to. `last` is the stops and position of
/// a recent press: with the same stops, go on to the next and wrap round;
/// otherwise start from the first.
pub fn tour_step(last: Option<(&[usize], usize)>, stops: &[usize]) -> usize {
	match last {
		Some((before, at)) if before == stops => (at + 1) % stops.len(),
		_ => 0,
	}
}

/// The page of AI Session keys that shows the sessions on `desktop`.
/// `page_of` is the page of each of `sessions` (see `Pages`).
pub fn page_showing(desktop: usize, sessions: &[(View, usize)], page_of: &[Option<usize>]) -> Option<usize> {
	sessions.iter().zip(page_of).find_map(|((_, on), page)| if *on == desktop { *page } else { None })
}

#[cfg(test)]
mod tests {
	use super::*;
	use Tool::{Claude, Codex};

	/// 3 columns: a Claude row over a Codex row.
	const LANES3: [Option<Tool>; 6] = [Some(Claude), Some(Claude), Some(Claude), Some(Codex), Some(Codex), Some(Codex)];

	fn keys(lanes: &[Option<Tool>], sessions: &[(Tool, usize)]) -> Vec<Vec<Option<usize>>> {
		paginate(lanes, sessions).keys
	}

	#[test]
	fn same_desktop_sits_in_one_column() {
		// desktop 0: Claude only; desktop 2: Claude + Codex; desktop 4: Codex only
		let sessions = [(Claude, 0), (Claude, 2), (Codex, 2), (Codex, 4)];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), Some(1), None, None, Some(2), Some(3)]]);
	}

	#[test]
	fn a_desktop_with_two_of_one_tool_takes_two_columns() {
		let sessions = [(Claude, 1), (Claude, 1), (Codex, 1), (Claude, 3)];
		assert_eq!(keys(&LANES3, &sessions), vec![vec![Some(0), Some(1), Some(3), Some(2), None, None]]);
	}

	#[test]
	fn columns_run_on_to_the_next_page_without_splitting_a_desktop() {
		// desktop 2 needs 2 columns but only 1 is left on page 1
		let sessions = [(Claude, 0), (Claude, 1), (Claude, 2), (Claude, 2), (Codex, 2)];
		let pages = paginate(&LANES3, &sessions);
		assert_eq!(
			pages.keys,
			vec![vec![Some(0), Some(1), None, None, None, None], vec![Some(2), Some(3), None, Some(4), None, None]]
		);
		assert_eq!(pages.page_of, vec![Some(0), Some(0), Some(1), Some(1), Some(1)]);
	}

	#[test]
	fn a_tool_without_a_row_goes_to_keys_for_no_tool() {
		let lanes = [Some(Claude), Some(Claude), None];
		assert_eq!(keys(&lanes, &[(Codex, 0), (Claude, 0)]), vec![vec![Some(1), None, Some(0)]]);
	}

	#[test]
	fn keys_for_no_tool_list_everything_in_pages() {
		let sessions = [(Claude, 0), (Codex, 0), (Claude, 1)];
		assert_eq!(keys(&[None, None], &sessions), vec![vec![Some(0), Some(1)], vec![Some(2), None]]);
	}

	#[test]
	fn no_sessions_is_one_empty_page() {
		assert_eq!(keys(&LANES3, &[]), vec![vec![None; 6]]);
	}

	fn view(state: State, since: u64) -> View {
		View { tool: Claude, label: String::new(), state, stale: false, since, desktop: None }
	}

	#[test]
	fn summary_key_visits_the_most_urgent_first_and_each_desktop_once() {
		use State::*;
		let sessions = [
			(view(Done, 10), 4),
			(view(Working, 1), 0),     // needs nobody
			(view(Permission, 50), 2),
			(view(Permission, 40), 5), // waiting longer than the one on 2
			(view(Done, 20), 2),       // desktop 2 is already a stop
			(view(Error, 30), 1),
			(view(Idle, 1), 3),
		];
		assert_eq!(tour_stops(&sessions, Prefs::default()), vec![5, 2, 1, 4]);
	}

	#[test]
	fn summary_key_turns_the_deck_to_the_page_of_the_desktop_it_goes_to() {
		use State::*;
		// 3 columns a page: desktops 0, 1, 2 on page 1, desktop 5 on page 2
		let sessions = [(view(Done, 1), 0), (view(Done, 1), 1), (view(Done, 1), 2), (view(Permission, 1), 5)];
		let placed: Vec<_> = sessions.iter().map(|(s, d)| (s.tool, *d)).collect();
		let pages = paginate(&LANES3, &placed);
		assert_eq!(page_showing(5, &sessions, &pages.page_of), Some(1));
		assert_eq!(page_showing(1, &sessions, &pages.page_of), Some(0));
		assert_eq!(page_showing(4, &sessions, &pages.page_of), None); // nothing shown for it
	}

	#[test]
	fn summary_key_skips_tools_not_ticked_in_its_settings() {
		use State::*;
		let codex = |state, since| View { tool: Codex, ..view(state, since) };
		let sessions = [(view(Done, 10), 4), (codex(Permission, 5), 2), (codex(Done, 1), 4)];
		let only_claude = Prefs { jump_claude: true, jump_codex: false };
		assert_eq!(tour_stops(&sessions, only_claude), vec![4]);
		assert_eq!(tour_stops(&sessions, Prefs { jump_claude: false, jump_codex: true }), vec![2, 4]);
		assert!(tour_stops(&sessions, Prefs { jump_claude: false, jump_codex: false }).is_empty());
	}

	#[test]
	fn settings_checkboxes_sit_on_the_first_key_of_each_row() {
		let at = |lanes: &[Option<Tool>]| option_keys(lanes).map(|(_, key)| key);
		assert_eq!(at(&LANES3), [Some(0), Some(3)]);
		// no rows: the first two keys
		assert_eq!(at(&[None, None, None]), [Some(0), Some(1)]);
		// a Claude row only: Codex takes the first key set to no tool
		assert_eq!(at(&[Some(Claude), Some(Claude), None]), [Some(0), Some(2)]);
		assert_eq!(at(&[Some(Claude)]), [Some(0), None]);
	}

	#[test]
	fn summary_key_goes_on_to_the_next_stop_and_round_again() {
		let stops = [5, 2, 1];
		assert_eq!(tour_step(None, &stops), 0);
		assert_eq!(tour_step(Some((&stops, 0)), &stops), 1);
		assert_eq!(tour_step(Some((&stops, 2)), &stops), 0);
		// the stops changed since the last press: from the top
		assert_eq!(tour_step(Some((&[5, 2], 0)), &stops), 0);
	}
}
