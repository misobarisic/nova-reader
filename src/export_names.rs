//! Human-readable export names with portable, bounded UTF-8 components.
pub const MAX_FILENAME_BYTES: usize = 160;
pub const MAX_FOLDER_BYTES: usize = 96;
// Leave room for a u32 collision counter without losing chapter/position data.
const COUNTER_RESERVE: usize = 16;

fn clean(value: &str) -> String {
    let value: String = value
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                c
            }
        })
        .collect();
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches([' ', '.'])
        .to_owned()
}

fn shorten(value: &str, max: usize) -> &str {
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].trim_end_matches([' ', '.'])
}

fn reserved(value: &str) -> bool {
    let base = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.chars().count() == 1
                    && suffix
                        .chars()
                        .next()
                        .is_some_and(|c| matches!(c, '1'..='9' | '¹' | '²' | '³'))
            })
        })
}

fn title(value: &str, max: usize) -> String {
    let cleaned = clean(value);
    let value = shorten(&cleaned, max);
    if value.is_empty() {
        "Untitled".into()
    } else if reserved(value) {
        format!("_{}", shorten(value, max.saturating_sub(1)))
    } else {
        value.into()
    }
}

fn chapter(value: &str) -> String {
    let value = clean(value);
    let mut number = value.as_str();
    let lower = number.to_ascii_lowercase();
    for prefix in ["chapter", "ch.", "ch"] {
        if lower.starts_with(prefix) {
            let rest = &number[prefix.len()..];
            if rest
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c.is_ascii_digit())
            {
                number = rest.trim();
                break;
            }
        }
    }
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if !whole.is_empty()
        && whole.bytes().all(|c| c.is_ascii_digit())
        && fraction.bytes().all(|c| c.is_ascii_digit())
    {
        let whole = whole.trim_start_matches('0');
        let whole = if whole.is_empty() { "0" } else { whole };
        let result = if fraction.is_empty() {
            format!("{whole:0>4}")
        } else {
            format!("{whole:0>4}.{fraction}")
        };
        shorten(&result, 32).into()
    } else if number.is_empty() {
        "Unknown".into()
    } else {
        shorten(number, 32).into()
    }
}

fn extension(value: &str) -> &'static str {
    match value.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" => "jpg",
        "jpeg" => "jpeg",
        "webp" => "webp",
        "avif" => "avif",
        "jxl" => "jxl",
        _ => "png",
    }
}

/// `page` is one-based. Screencaps use the first visible page and its source
/// pixel Y coordinate; ordinary page exports pass None.
pub fn filename(
    series: &str,
    chapter_label: &str,
    page: usize,
    source_y: Option<u32>,
    ext: &str,
) -> String {
    let position = source_y.map(|y| format!(" - Y{y:06}")).unwrap_or_default();
    let suffix = format!(
        " - Ch. {} - P. {:04}{position}.{}",
        chapter(chapter_label),
        page.max(1),
        extension(ext)
    );
    let budget = MAX_FILENAME_BYTES - COUNTER_RESERVE - suffix.len();
    format!("{}{suffix}", title(series, budget))
}

pub fn folder(series: &str) -> String {
    title(series, MAX_FOLDER_BYTES)
}

/// Call with the original filename on every retry, rather than nesting counters.
pub fn numbered(filename: &str, number: u32) -> String {
    let (stem, ext) = filename.rsplit_once('.').unwrap_or((filename, "png"));
    let suffix = format!(" ({}).{}", number.max(2), extension(ext));
    format!("{}{suffix}", title(stem, MAX_FILENAME_BYTES - suffix.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_chapter_page_and_vertical_position_sort_naturally() {
        assert_eq!(
            filename(
                "The Café Terrace and Its Goddesses",
                "Ch. 1.5",
                17,
                Some(12480),
                "avif"
            ),
            "The Café Terrace and Its Goddesses - Ch. 0001.5 - P. 0017 - Y012480.avif"
        );
        let names = [(1, 2, None), (1, 17, None), (999, 1, None), (1000, 1, None)]
            .map(|(ch, page, y)| filename("Story", &ch.to_string(), page, y, "jpg"));
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            filename("Story", "1", 1, Some(9), "png") < filename("Story", "1", 1, Some(100), "png")
        );
        assert!(filename("Story", "1", 1, None, "png").contains("P. 0001.png"));
    }

    #[test]
    fn unicode_titles_and_duplicate_suffixes_stay_within_real_byte_limits() {
        let series = "読書📚Café".repeat(100);
        let name = filename(&series, "652.001", 4096, Some(65535), "webp");
        assert!(name.len() <= MAX_FILENAME_BYTES);
        assert!(name.ends_with(" - Ch. 0652.001 - P. 4096 - Y065535.webp"));
        assert_eq!(String::from_utf8(name.as_bytes().to_vec()).unwrap(), name);
        for counter in [2, 999, u32::MAX] {
            let duplicate = numbered(&name, counter);
            assert!(duplicate.len() <= MAX_FILENAME_BYTES);
            assert!(duplicate.ends_with(&format!(" ({counter}).webp")));
            assert!(duplicate.contains("Ch. 0652.001 - P. 4096 - Y065535"));
        }
        assert!(folder(&series).len() <= MAX_FOLDER_BYTES);
    }

    #[test]
    fn paths_controls_and_reserved_folder_names_are_safe() {
        assert_eq!(folder("../ CON.txt. "), "_CON.txt");
        assert_eq!(folder("LPT¹"), "_LPT¹");
        assert_eq!(folder(" .\u{202e}\n<>:*? "), "Untitled");
        let name = filename("../A/B\\C\u{202e}:?", "Special / one-shot", 0, None, ".PNG");
        assert!(!name.contains(['/', '\\', ':', '?', '\u{202e}']));
        assert!(name.starts_with("A B C - Ch. Special one-shot"));
        assert!(name.ends_with("P. 0001.png"));
    }
}
