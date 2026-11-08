//! Text and figures for chapter rows on the detail screen: the number column,
//! a title without the repeated "Chapter N" prefix, the coloured status that
//! leads the details line, and reading progress. Reading progress stores only
//! the page reached, so a partly read row says "Page X"; its bar needs the
//! chapter's length, which only a download's page list provides.
use nova_reader_ui::backend_text as text;

/// The title without a leading "Chapter N" (or "Ch. N", "Episode N") that
/// only repeats the number column. A title that is nothing but that prefix is
/// kept whole, so the row never goes blank.
pub fn short_title(title: &str, number: Option<&str>) -> String {
    let Some(number) = number.map(str::trim).filter(|n| !n.is_empty()) else {
        return title.to_owned();
    };
    let trimmed = title.trim_start();
    let lower = trimmed.to_lowercase();
    // Offsets below are shared between the two spellings; a title whose
    // lowercase form changes length keeps its full text.
    if lower.len() != trimmed.len() {
        return title.to_owned();
    }
    for word in ["chapter", "episode", "ch.", "ch", "ep.", "ep"] {
        let Some(rest) = lower.strip_prefix(word) else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(after) = rest.strip_prefix(&number.to_lowercase()) else {
            continue;
        };
        // "Chapter 1" must not match "Chapter 12".
        if after.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let after = after.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ':' | '-' | '–' | '—' | '.' | '|')
        });
        if after.is_empty() {
            return title.to_owned();
        }
        // `after` is a suffix of `lower`, which has `trimmed`'s byte length.
        let start = trimmed.len() - after.len();
        return trimmed
            .get(start..)
            .map_or_else(|| title.to_owned(), str::to_owned);
    }
    title.to_owned()
}

/// The status that leads a row's details line, and whether it is an error.
/// Download state codes follow `ItemRow::download-state`.
pub fn status(
    download_state: i32,
    download_fraction: f32,
    page: Option<usize>,
    gallery: bool,
) -> Option<(String, bool)> {
    let percent = (download_fraction.clamp(0., 1.) * 100.).round() as u32;
    match download_state {
        2 => return Some((format!("{} {percent}%", text::tr("Downloading")), false)),
        1 => return Some((text::tr("Queued"), false)),
        3 => return Some((format!("{} {percent}%", text::tr("Paused")), false)),
        4 => return Some((text::tr("Failed"), true)),
        _ => {}
    }
    let index = page?;
    let noun = text::tr(if gallery { "Image" } else { "Page" });
    Some((format!("{noun} {}", index + 1), false))
}

/// How far into a partly read chapter the reader is, when its length is known.
pub fn read_fraction(page: Option<usize>, total: Option<usize>) -> f32 {
    match (page, total) {
        (Some(index), Some(total)) if total > 0 => {
            ((index + 1) as f32 / total as f32).clamp(0., 1.)
        }
        _ => 0.,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_title_drops_only_a_repeated_number_prefix() {
        let cases = [
            ("Chapter 12: Night Survey", Some("12"), "Night Survey"),
            ("Ch. 12 - Night Survey", Some("12"), "Night Survey"),
            ("chapter 12.5 — Interlude", Some("12.5"), "Interlude"),
            ("Episode 3 Copper Wind", Some("3"), "Copper Wind"),
            // Another number, or no number, keeps the title.
            (
                "Chapter 12: Night Survey",
                Some("1"),
                "Chapter 12: Night Survey",
            ),
            ("Chapter 12: Night Survey", None, "Chapter 12: Night Survey"),
            ("Night Survey", Some("12"), "Night Survey"),
            // Nothing but the prefix stays whole.
            ("Chapter 12", Some("12"), "Chapter 12"),
            ("Chaptered 12", Some("12"), "Chaptered 12"),
        ];
        for (title, number, expected) in cases {
            assert_eq!(short_title(title, number), expected, "{title}");
        }
    }

    #[test]
    fn status_follows_download_priority_then_reading_position() {
        let page = Some(13);
        assert_eq!(
            status(2, 0.35, page, false),
            Some(("Downloading 35%".into(), false))
        );
        assert_eq!(status(1, 0., page, false), Some(("Queued".into(), false)));
        assert_eq!(
            status(3, 0.6, page, false),
            Some(("Paused 60%".into(), false))
        );
        assert_eq!(status(4, 0., page, false), Some(("Failed".into(), true)));
        assert_eq!(status(0, 0., page, false), Some(("Page 14".into(), false)));
        assert_eq!(
            status(5, 1., Some(2), true),
            Some(("Image 3".into(), false))
        );
        assert_eq!(status(0, 0., None, false), None);
        assert!((read_fraction(page, Some(32)) - 14. / 32.).abs() < 1e-6);
        assert_eq!(read_fraction(page, None), 0.);
    }
}
