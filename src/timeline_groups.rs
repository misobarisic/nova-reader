//! Updates "Grouped" and History "Combined": dated chapter rows grouped by
//! local day, then by series, and flattened into lines of known kinds and
//! heights so the Slint list keeps creating only the rows near its viewport.
//!
//! The shapes are pure so ordering, ranges, expansion and sessions are
//! unit-tested here; `app.rs` builds the facts and publishes the lines.
use nova_reader_ui::backend_text as text;
use std::collections::HashMap;

/// One dated chapter, in display order (newest first).
#[derive(Clone, Debug, Default)]
pub(crate) struct Fact {
    /// Index into the view's rows: open, download and remove use it.
    pub row: usize,
    /// Stable series key; groups never merge two series with one title.
    pub series: String,
    pub series_title: String,
    pub artwork_key: String,
    pub chapter_title: String,
    pub number: Option<String>,
    /// Local calendar day (Julian day number); `None` for unknown dates.
    pub day_key: Option<i32>,
    /// "Today", "Yesterday" or the date.
    pub day: String,
    /// Short date for the day subtitle ("Sat 8 Nov").
    pub date: String,
    /// "HH:MM" local time; empty when unknown.
    pub time: String,
    pub read: bool,
    pub download_state: i32,
    pub download_progress: f32,
    /// Zero-based page reached in an unfinished chapter.
    pub page: Option<usize>,
    /// Page count, when a download records it.
    pub pages: Option<usize>,
}

impl Fact {
    fn downloaded(&self) -> bool {
        self.download_state == 5
    }
    fn number_value(&self) -> Option<f64> {
        self.number.as_deref()?.trim().parse::<f64>().ok()
    }
}

/// Line kinds, shared with the Slint list as `TimelineEntry.kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Day = 0,
    GroupHeader = 1,
    Chapter = 2,
    ShowMore = 3,
    GroupActions = 4,
    Single = 5,
    TimelineItem = 6,
}

impl Kind {
    /// Visual height in logical pixels. The list positions lines from these,
    /// so they must match `timeline-page.slint`.
    pub(crate) fn height(self, history: bool, timeline: bool) -> f32 {
        match self {
            Kind::Day if timeline => 36.,
            Kind::Day => 44.,
            Kind::GroupHeader => 84.,
            Kind::Chapter => 48.,
            Kind::ShowMore => 42.,
            Kind::GroupActions => 50.,
            Kind::Single if history => 84.,
            Kind::Single => 84.,
            Kind::TimelineItem => 64.,
        }
    }
}

/// How much of a group is open. Kept per (day, series) for the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expansion {
    /// Header only.
    Collapsed,
    /// The first [`PEEK`] chapters and "Show N more".
    Peek,
    /// Every chapter.
    Full,
}

pub(crate) type ExpansionState = HashMap<String, Expansion>;

/// Chapters shown in a collapsed-to-peek Updates group.
pub(crate) const PEEK: usize = 3;
/// Space between cards, below the last line of each.
pub(crate) const CARD_GAP: f32 = 8.;
/// History's "Pick up where you left off" holds at most this many series.
pub(crate) const SHELF: usize = 6;

/// One flattened line.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Line {
    pub kind: Kind,
    /// Expansion key of the group or session ("u:day|series" / "h:…").
    pub key: String,
    /// Facts covered: the chapter itself, a group, a session or a day.
    pub facts: Vec<usize>,
    pub expanded: bool,
    /// "Show N more": how many are hidden.
    pub hidden: usize,
    /// Closes a card: bottom corners and the gap below.
    pub last: bool,
    pub y: f32,
    pub height: f32,
}

/// A day, then its series in order of their newest chapter.
pub(crate) struct Day {
    pub facts: Vec<usize>,
    pub groups: Vec<Vec<usize>>,
}

/// Group facts (already newest first) by day, then series. Order follows
/// first appearance, so a series sits where its newest chapter does.
pub(crate) fn days(facts: &[Fact]) -> Vec<Day> {
    let mut days: Vec<(Option<i32>, Day)> = Vec::new();
    for (index, fact) in facts.iter().enumerate() {
        let at = match days.iter().position(|(key, _)| *key == fact.day_key) {
            Some(at) => at,
            None => {
                days.push((
                    fact.day_key,
                    Day {
                        facts: Vec::new(),
                        groups: Vec::new(),
                    },
                ));
                days.len() - 1
            }
        };
        let day = &mut days[at].1;
        day.facts.push(index);
        match day
            .groups
            .iter_mut()
            .find(|group| facts[group[0]].series == fact.series)
        {
            Some(group) => group.push(index),
            None => day.groups.push(vec![index]),
        }
    }
    days.into_iter().map(|(_, day)| day).collect()
}

fn group_key(prefix: &str, fact: &Fact) -> String {
    format!(
        "{prefix}:{}|{}",
        fact.day_key.map_or("?".into(), |d| d.to_string()),
        fact.series
    )
}

struct Builder {
    lines: Vec<Line>,
    y: f32,
    history: bool,
    timeline: bool,
}

impl Builder {
    fn new(history: bool, timeline: bool, top: f32) -> Self {
        Self {
            lines: Vec::new(),
            y: top,
            history,
            timeline,
        }
    }
    fn push(&mut self, kind: Kind, key: String, facts: Vec<usize>) -> &mut Line {
        let height = kind.height(self.history, self.timeline);
        self.lines.push(Line {
            kind,
            key,
            facts,
            expanded: false,
            hidden: 0,
            last: false,
            y: self.y,
            height,
        });
        self.y += height;
        self.lines.last_mut().unwrap()
    }
    /// Close the card that the last line belongs to.
    fn close(&mut self) {
        if let Some(line) = self.lines.last_mut() {
            line.last = true;
        }
        self.y += CARD_GAP;
    }
}

/// Updates. `grouped == false` is "By day only": today's flat list of
/// single rows under day headings.
pub(crate) fn updates(
    facts: &[Fact],
    grouped: bool,
    state: &ExpansionState,
    top: f32,
) -> (Vec<Line>, f32) {
    let mut out = Builder::new(false, false, top);
    for day in days(facts) {
        out.push(Kind::Day, String::new(), day.facts.clone());
        for group in day.groups {
            if !grouped {
                for fact in group {
                    out.push(Kind::Single, String::new(), vec![fact]);
                    out.close();
                }
                continue;
            }
            if group.len() == 1 {
                out.push(Kind::Single, String::new(), group);
                out.close();
                continue;
            }
            let key = group_key("u", &facts[group[0]]);
            // Small groups have nothing to hide: they open fully.
            let default = if group.len() <= PEEK {
                Expansion::Full
            } else {
                Expansion::Peek
            };
            let expansion = state.get(&key).copied().unwrap_or(default);
            let expansion = if group.len() <= PEEK && expansion == Expansion::Peek {
                Expansion::Full
            } else {
                expansion
            };
            out.push(Kind::GroupHeader, key.clone(), group.clone())
                .expanded = expansion == Expansion::Full;
            if expansion == Expansion::Collapsed {
                out.close();
                continue;
            }
            let shown = if expansion == Expansion::Peek {
                PEEK
            } else {
                group.len()
            };
            for &fact in &group[..shown] {
                out.push(Kind::Chapter, key.clone(), vec![fact]);
            }
            if shown < group.len() {
                out.push(Kind::ShowMore, key.clone(), group[shown..].to_vec())
                    .hidden = group.len() - shown;
            }
            out.push(Kind::GroupActions, key, group);
            out.close();
        }
    }
    (out.lines, out.y)
}

/// History as session cards: one card per series per day, collapsed by
/// default; a one-chapter session is a single row.
pub(crate) fn history_cards(facts: &[Fact], state: &ExpansionState, top: f32) -> (Vec<Line>, f32) {
    let mut out = Builder::new(true, false, top);
    for day in days(facts) {
        out.push(Kind::Day, String::new(), day.facts.clone());
        for session in day.groups {
            if session.len() == 1 {
                out.push(Kind::Single, String::new(), session);
                out.close();
                continue;
            }
            let key = group_key("h", &facts[session[0]]);
            let open = state.get(&key) == Some(&Expansion::Full);
            out.push(Kind::GroupHeader, key.clone(), session.clone())
                .expanded = open;
            if open {
                for &fact in &session {
                    out.push(Kind::Chapter, key.clone(), vec![fact]);
                }
                out.push(Kind::GroupActions, key, session);
            }
            out.close();
        }
    }
    (out.lines, out.y)
}

/// History as a timeline: one item per session under each day.
pub(crate) fn history_timeline(facts: &[Fact], top: f32) -> (Vec<Line>, f32) {
    let mut out = Builder::new(true, true, top);
    for day in days(facts) {
        out.push(Kind::Day, String::new(), day.facts.clone());
        for session in day.groups {
            let key = group_key("h", &facts[session[0]]);
            out.push(Kind::TimelineItem, key, session);
        }
        out.y += CARD_GAP;
    }
    (out.lines, out.y)
}

/// Header tap: a full group goes back to its peek, a peek closes to the
/// header and a closed group reopens to its peek. Small groups and History
/// sessions have no peek and toggle between open and closed.
pub(crate) fn toggle(state: &mut ExpansionState, key: &str, size: usize, history: bool) {
    let small = history || size <= PEEK;
    let current = state.get(key).copied().unwrap_or(if history {
        Expansion::Collapsed
    } else if small {
        Expansion::Full
    } else {
        Expansion::Peek
    });
    let next = match (current, small) {
        (Expansion::Full, true) => Expansion::Collapsed,
        (_, true) => Expansion::Full,
        (Expansion::Full, false) => Expansion::Peek,
        (Expansion::Peek, false) => Expansion::Collapsed,
        (Expansion::Collapsed, false) => Expansion::Peek,
    };
    state.insert(key.to_owned(), next);
}

/// A contiguous whole-number span of chapters, lowest first. Decimal,
/// missing or gapped numbers have no range.
pub(crate) fn range(facts: &[Fact], members: &[usize]) -> Option<(String, String)> {
    let mut numbers = members
        .iter()
        .map(|&i| facts[i].number_value())
        .collect::<Option<Vec<_>>>()?;
    if numbers.iter().any(|n| n.fract() != 0.) {
        return None;
    }
    numbers.sort_by(f64::total_cmp);
    numbers.dedup();
    if numbers.len() != members.len() || numbers.windows(2).any(|w| w[1] - w[0] != 1.) {
        return None;
    }
    let label = |n: f64| format!("{n:.0}");
    Some((label(numbers[0]), label(*numbers.last()?)))
}

/// "Chapters 41–54", "Chapter 12", or nothing when there is no range.
pub(crate) fn range_label(facts: &[Fact], members: &[usize]) -> Option<String> {
    let (first, last) = range(facts, members)?;
    Some(if first == last {
        format!("{} {first}", text::tr("Chapter"))
    } else {
        format!("{} {first}–{last}", text::tr("Chapters"))
    })
}

/// The accent part and the rest of an Updates group line:
/// ("14 new", "Chapters 41–54"), or ("3 new chapters", "") without a range.
pub(crate) fn updates_summary(facts: &[Fact], members: &[usize]) -> (String, String) {
    let n = members.len();
    match range_label(facts, members) {
        Some(range) => (format!("{n} {}", text::tr("new")), range),
        None => (format!("{n} {}", text::tr("new chapters")), String::new()),
    }
}

/// "6 chapters" and "Chapters 82–87 · 20:04–21:42" for a History session.
pub(crate) fn session_summary(facts: &[Fact], members: &[usize]) -> (String, String) {
    let mut rest = Vec::new();
    if let Some(range) = range_label(facts, members) {
        rest.push(range);
    }
    let newest = &facts[members[0]].time;
    let oldest = &facts[*members.last().unwrap_or(&members[0])].time;
    if !newest.is_empty() {
        rest.push(if oldest == newest || oldest.is_empty() {
            newest.clone()
        } else {
            format!("{oldest}–{newest}")
        });
    }
    (text::chapters_label(members.len()), rest.join(" · "))
}

/// "Sat 8 Nov · 18 chapters".
pub(crate) fn day_subtitle(facts: &[Fact], day: &[usize]) -> String {
    let date = &facts[day[0]].date;
    let count = text::chapters_label(day.len());
    if date.is_empty() {
        count
    } else {
        format!("{date} · {count}")
    }
}

/// Reading progress through a chapter, when it is unfinished and its length
/// is known.
pub(crate) fn progress(fact: &Fact) -> Option<f32> {
    let page = fact.page?;
    let pages = fact.pages.filter(|n| *n > 0)?;
    Some(((page + 1) as f32 / pages as f32).clamp(0., 1.))
}

/// The oldest unread chapter of a group: "Read {first new}" opens it.
pub(crate) fn first_unread(facts: &[Fact], members: &[usize]) -> Option<usize> {
    members.iter().rev().copied().find(|&i| !facts[i].read)
}

/// Chips: All, Unread, Downloaded. The filter runs before grouping.
pub(crate) fn passes(fact: &Fact, filter: i32) -> bool {
    match filter {
        1 => !fact.read,
        2 => fact.downloaded(),
        _ => true,
    }
}

pub(crate) fn filter_counts(facts: &[Fact]) -> [i32; 3] {
    [0, 1, 2].map(|filter| facts.iter().filter(|f| passes(f, filter)).count() as i32)
}

/// One series per shelf item, from the most recent sessions.
pub(crate) fn shelf(facts: &[Fact]) -> Vec<usize> {
    let mut seen = Vec::<&str>::new();
    let mut out = Vec::new();
    for (index, fact) in facts.iter().enumerate() {
        if seen.contains(&fact.series.as_str()) {
            continue;
        }
        seen.push(&fact.series);
        out.push(index);
        if out.len() == SHELF {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(series: &str, number: &str, day: i32, read: bool) -> Fact {
        Fact {
            series: series.into(),
            series_title: series.to_uppercase(),
            number: Some(number.into()),
            chapter_title: format!("Chapter {number}"),
            day_key: Some(day),
            day: format!("day {day}"),
            date: format!("date {day}"),
            read,
            ..Default::default()
        }
    }

    fn burst(series: &str, first: u32, last: u32, day: i32) -> Vec<Fact> {
        (first..=last)
            .rev()
            .map(|n| fact(series, &n.to_string(), day, false))
            .collect()
    }

    fn kinds(lines: &[Line]) -> Vec<Kind> {
        lines.iter().map(|l| l.kind).collect()
    }

    #[test]
    fn a_burst_groups_into_one_peeking_card() {
        let facts = burst("a", 41, 54, 10);
        let (lines, _) = updates(&facts, true, &ExpansionState::new(), 0.);
        use Kind::*;
        assert_eq!(
            kinds(&lines),
            [
                Day,
                GroupHeader,
                Chapter,
                Chapter,
                Chapter,
                ShowMore,
                GroupActions
            ]
        );
        assert_eq!(lines[5].hidden, 11);
        assert!(lines[6].last);
        assert_eq!(
            updates_summary(&facts, &lines[1].facts),
            ("14 new".into(), "Chapters 41–54".into())
        );
        // Newest first inside the group; "Read" starts at the oldest unread.
        assert_eq!(facts[lines[2].facts[0]].number.as_deref(), Some("54"));
        assert_eq!(
            facts[first_unread(&facts, &lines[1].facts).unwrap()]
                .number
                .as_deref(),
            Some("41")
        );
        // Positions follow the heights, with the gap after the card.
        assert_eq!(lines[1].y, 44.);
        let (_, end) = updates(&facts, true, &ExpansionState::new(), 0.);
        assert_eq!(end, 44. + 84. + 3. * 48. + 42. + 50. + CARD_GAP);
    }

    #[test]
    fn several_series_and_days_keep_their_order() {
        let mut facts = Vec::new();
        facts.extend(burst("a", 10, 11, 10));
        facts.push(fact("b", "5", 10, false));
        facts.extend(burst("c", 1, 4, 10));
        // The same series again on an older day is a separate group.
        facts.extend(burst("a", 8, 9, 9));
        let found = days(&facts);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].groups.len(), 3);
        assert_eq!(found[0].facts.len(), 7);
        assert_eq!(found[1].groups, vec![vec![7, 8]]);
        let (lines, _) = updates(&facts, true, &ExpansionState::new(), 0.);
        use Kind::*;
        assert_eq!(
            kinds(&lines),
            [
                Day,
                // a: two chapters open fully, no "more".
                GroupHeader,
                Chapter,
                Chapter,
                GroupActions,
                // b: one chapter is a single row.
                Single,
                // c: four chapters peek at three.
                GroupHeader,
                Chapter,
                Chapter,
                Chapter,
                ShowMore,
                GroupActions,
                Day,
                GroupHeader,
                Chapter,
                Chapter,
                GroupActions,
            ]
        );
        assert_eq!(
            day_subtitle(&facts, &found[0].facts),
            "date 10 · 7 chapters"
        );
    }

    #[test]
    fn ranges_fall_back_when_numbers_are_not_contiguous_whole_numbers() {
        let mut facts = burst("a", 1, 3, 1);
        assert_eq!(
            range_label(&facts, &[0, 1, 2]).as_deref(),
            Some("Chapters 1–3")
        );
        assert_eq!(range_label(&facts, &[1]).as_deref(), Some("Chapter 2"));
        facts[1].number = Some("2.5".into());
        assert_eq!(range_label(&facts, &[0, 1, 2]), None);
        facts[1].number = Some("extra".into());
        assert_eq!(range_label(&facts, &[0, 1, 2]), None);
        facts[1].number = None;
        assert_eq!(range_label(&facts, &[0, 1, 2]), None);
        let gapped = vec![fact("a", "1", 1, false), fact("a", "3", 1, false)];
        assert_eq!(range_label(&gapped, &[0, 1]), None);
        assert_eq!(
            updates_summary(&gapped, &[0, 1]),
            ("2 new chapters".into(), String::new())
        );
    }

    #[test]
    fn expansion_survives_rebuilds_and_cycles() {
        let facts = burst("a", 1, 14, 3);
        let mut state = ExpansionState::new();
        let header = |state: &ExpansionState| updates(&facts, true, state, 0.).0[1].clone();
        let key = header(&state).key;
        // "Show 11 more" opens everything.
        state.insert(key.clone(), Expansion::Full);
        let (lines, _) = updates(&facts, true, &state, 0.);
        assert_eq!(lines.iter().filter(|l| l.kind == Kind::Chapter).count(), 14);
        assert!(!lines.iter().any(|l| l.kind == Kind::ShowMore));
        assert!(header(&state).expanded);
        // A header tap goes back to three, then to the header alone.
        toggle(&mut state, &key, 14, false);
        let (lines, _) = updates(&facts, true, &state, 0.);
        assert_eq!(lines.iter().filter(|l| l.kind == Kind::Chapter).count(), 3);
        toggle(&mut state, &key, 14, false);
        let (lines, _) = updates(&facts, true, &state, 0.);
        assert_eq!(kinds(&lines), [Kind::Day, Kind::GroupHeader]);
        assert!(lines[1].last);
        toggle(&mut state, &key, 14, false);
        assert_eq!(state[&key], Expansion::Peek);
        // Small groups toggle between open and closed.
        let mut small = ExpansionState::new();
        toggle(&mut small, "k", 2, false);
        assert_eq!(small["k"], Expansion::Collapsed);
        toggle(&mut small, "k", 2, false);
        assert_eq!(small["k"], Expansion::Full);
    }

    #[test]
    fn day_only_is_a_flat_list_and_filters_run_first() {
        let mut facts = burst("a", 1, 4, 2);
        facts[0].read = true;
        facts[1].download_state = 5;
        let (lines, _) = updates(&facts, false, &ExpansionState::new(), 0.);
        assert_eq!(
            kinds(&lines),
            [
                Kind::Day,
                Kind::Single,
                Kind::Single,
                Kind::Single,
                Kind::Single
            ]
        );
        assert_eq!(filter_counts(&facts), [4, 3, 1]);
        let mut all_read = burst("b", 1, 2, 2);
        for f in &mut all_read {
            f.read = true;
        }
        let unread: Vec<_> = all_read.into_iter().filter(|f| passes(f, 1)).collect();
        assert!(
            updates(&unread, true, &ExpansionState::new(), 0.)
                .0
                .is_empty()
        );
    }

    #[test]
    fn history_sessions_split_by_series_and_day() {
        // Two series interleaved on one day, and a midnight boundary: the
        // day key comes from local time, so 23:55 and 00:05 differ.
        let mut facts = vec![
            fact("a", "87", 20, true),
            fact("b", "12", 20, true),
            fact("a", "86", 20, true),
            fact("a", "85", 20, true),
            fact("a", "84", 19, true),
        ];
        facts[0].time = "21:42".into();
        facts[2].time = "21:20".into();
        facts[3].time = "20:04".into();
        let found = days(&facts);
        assert_eq!(found[0].groups, vec![vec![0, 2, 3], vec![1]]);
        assert_eq!(found[1].groups, vec![vec![4]]);
        assert_eq!(
            session_summary(&facts, &found[0].groups[0]),
            ("3 chapters".into(), "Chapters 85–87 · 20:04–21:42".into())
        );
        let mut state = ExpansionState::new();
        let (lines, _) = history_cards(&facts, &state, 0.);
        use Kind::*;
        assert_eq!(kinds(&lines), [Day, GroupHeader, Single, Day, Single]);
        toggle(&mut state, &lines[1].key, 3, true);
        let (lines, _) = history_cards(&facts, &state, 0.);
        assert_eq!(
            kinds(&lines),
            [
                Day,
                GroupHeader,
                Chapter,
                Chapter,
                Chapter,
                GroupActions,
                Single,
                Day,
                Single
            ]
        );
        // Both views show the same sessions.
        let (timeline, _) = history_timeline(&facts, 0.);
        assert_eq!(
            kinds(&timeline),
            [Day, TimelineItem, TimelineItem, Day, TimelineItem]
        );
        assert_eq!(timeline[1].facts, vec![0, 2, 3]);
    }

    #[test]
    fn shelf_takes_one_series_each_up_to_six() {
        let facts: Vec<_> = ["a", "a", "b", "c", "b", "d", "e", "f", "g"]
            .iter()
            .map(|s| fact(s, "1", 1, true))
            .collect();
        let picked = shelf(&facts);
        assert_eq!(picked, vec![0, 2, 3, 5, 6, 7]);
        let mut f = fact("a", "1", 1, false);
        f.page = Some(8);
        f.pages = Some(30);
        assert_eq!(progress(&f), Some(0.3));
        f.pages = None;
        assert_eq!(progress(&f), None);
    }
}
