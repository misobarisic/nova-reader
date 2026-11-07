//! Read-ahead for ordinary chapters: whole (unsplit) pages in webtoon and
//! paged modes, in both directions, and across the chapter boundary. Each test
//! drives the real image worker and waits for it to go idle after every
//! gesture, the way a reader dwells between swipes.
use super::reader_handoff_tests::{LiveReader, whole_page};
use super::*;

fn retained(scroll: &LiveReader) -> BTreeSet<usize> {
    scroll.app.view().reader.as_ref().unwrap().retained.clone()
}

/// Pages whose rows lie within `ahead` layout pixels beyond the viewport edge
/// in the scroll direction.
fn pages_within(scroll: &LiveReader, at: f32, backwards: bool, ahead: f32) -> Vec<usize> {
    let view = scroll.app.view();
    let reader = view.reader.as_ref().unwrap();
    let (low, high) = if backwards {
        (at - ahead, at)
    } else {
        (at + 800., at + 800. + ahead)
    };
    (0..reader.pages.len())
        .filter(|&i| {
            let top = reader.page_position(i) * 512.;
            let bottom = top + reader.ratios[i] * 512.;
            bottom > low && top < high
        })
        .collect()
}

/// Webtoon with ordinary pages: the pages up to two viewport heights ahead of
/// the viewport are decoded before it reaches them, scrolling down and back up.
#[test]
fn webtoon_decodes_ordinary_pages_two_viewports_ahead_in_both_directions() {
    if !super::reader_handoff_tests::in_child(
        "app::reader_read_ahead_tests::webtoon_decodes_ordinary_pages_two_viewports_ahead_in_both_directions",
        "READER_WEBTOON_AHEAD_CHILD",
    ) {
        return;
    }
    let scroll = LiveReader::start(
        |dir| (0..14).map(|i| whole_page(dir, i, 512, 1000)).collect(),
        (0, 0.),
    );
    let mut missing = Vec::new();
    let mut at = 0.;
    for _ in 0..8 {
        at += 800.;
        scroll.scroll_to(at);
        let have = retained(&scroll);
        for page in pages_within(&scroll, at, false, 1600.) {
            if !have.contains(&page) {
                missing.push(("down", at, page));
            }
        }
    }
    for _ in 0..6 {
        at -= 800.;
        scroll.scroll_to(at);
        let have = retained(&scroll);
        for page in pages_within(&scroll, at, true, 1600.) {
            if !have.contains(&page) {
                missing.push(("up", at, page));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "not decoded ahead of the viewport: {missing:?}"
    );
}

/// Paged modes show one page, but flipping quickly outruns a one-page lookahead:
/// the next two pages in the reading direction are decoded while the budget
/// allows, one page stays behind.
fn paged_case(test: &str, marker: &str, mode: ReadingMode) {
    if !super::reader_handoff_tests::in_child(
        &format!("app::reader_read_ahead_tests::{test}"),
        marker,
    ) {
        return;
    }
    let scroll = LiveReader::start_in(
        mode,
        |dir| (0..9).map(|i| whole_page(dir, i, 512, 768)).collect(),
        (0, 0.),
    );
    let mut missing = Vec::new();
    for step in [1, 1, 1, -1, -1, 1] {
        scroll.app.step_page(&scroll.ui, step);
        super::reader_handoff_tests::settle_and_wait_idle(&scroll.app);
        let (index, have, backwards) = {
            let view = scroll.app.view();
            let r = view.reader.as_ref().unwrap();
            (
                r.anchor.page_index,
                r.retained.clone(),
                r.continuity_backwards,
            )
        };
        let wanted = if backwards {
            [index.checked_sub(1), index.checked_sub(2)]
        } else {
            [Some(index + 1), Some(index + 2)]
        };
        for page in wanted.into_iter().flatten().filter(|p| *p < 9) {
            if !have.contains(&page) {
                missing.push((mode, index, page));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "pages not decoded two ahead in the reading direction: {missing:?}"
    );
}
#[test]
fn left_to_right_decodes_two_pages_ahead() {
    paged_case(
        "left_to_right_decodes_two_pages_ahead",
        "READER_PAGED_LTR_CHILD",
        ReadingMode::LeftToRight,
    );
}
#[test]
fn right_to_left_decodes_two_pages_ahead() {
    paged_case(
        "right_to_left_decodes_two_pages_ahead",
        "READER_PAGED_RTL_CHILD",
        ReadingMode::RightToLeft,
    );
}
#[test]
fn vertical_paged_decodes_two_pages_ahead() {
    paged_case(
        "vertical_paged_decodes_two_pages_ahead",
        "READER_PAGED_VERTICAL_CHILD",
        ReadingMode::Vertical,
    );
}

/// Two chapters in one page list (the next chapter's metadata has arrived):
/// its first pages are decoded before the viewer crosses the boundary, in
/// either direction, exactly like pages within one chapter.
#[test]
fn the_next_chapters_first_pages_are_decoded_before_the_boundary() {
    if !super::reader_handoff_tests::in_child(
        "app::reader_read_ahead_tests::the_next_chapters_first_pages_are_decoded_before_the_boundary",
        "READER_CHAPTER_AHEAD_CHILD",
    ) {
        return;
    }
    let scroll = LiveReader::start_configured(
        ReadingMode::Webtoon,
        |dir| (0..10).map(|i| whole_page(dir, i, 512, 1000)).collect(),
        (0, 0.),
        |reader| {
            let series = reader.chapter.series.clone();
            let next = ChapterId::new(series, "chapter-2");
            reader.segments = vec![
                reader_continuity::ChapterSegment {
                    chapter: reader.chapter.clone(),
                    title: "One".into(),
                    start: 0,
                    end: 5,
                    source: None,
                    completed: false,
                },
                reader_continuity::ChapterSegment {
                    chapter: next,
                    title: "Two".into(),
                    start: 5,
                    end: 10,
                    source: None,
                    completed: false,
                },
            ];
        },
    );
    let boundary = scroll.page_top(5);
    let mut at = 0.;
    let mut missing = Vec::new();
    while at + 800. < boundary {
        at += 800.;
        scroll.scroll_to(at);
        let have = retained(&scroll);
        for page in pages_within(&scroll, at, false, 1600.) {
            if !have.contains(&page) {
                missing.push(("down", at, page));
            }
        }
    }
    assert!(retained(&scroll).contains(&5), "chapter two's first page");
    // Cross, then come back: the previous chapter's last pages stay ready.
    while at < boundary + 3000. {
        at += 800.;
        scroll.scroll_to(at);
    }
    while at > boundary - 400. {
        at -= 800.;
        scroll.scroll_to(at);
        let have = retained(&scroll);
        for page in pages_within(&scroll, at, true, 1600.) {
            if !have.contains(&page) {
                missing.push(("up", at, page));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "not decoded across the boundary: {missing:?}"
    );
}

fn reads() -> Vec<String> {
    super::reader_handoff_tests::PAGE_READS
        .lock()
        .unwrap()
        .clone()
}

/// Fetching is slower than decoding on a network source, so the encoded bytes
/// of the pages beyond the decode window are fetched while the viewer is idle
/// (a few pages, bounded in bytes), in the scroll direction, without decoding
/// them, and each page is fetched once: its decode finds the bytes waiting.
#[test]
fn encoded_bytes_are_fetched_beyond_the_decode_window_in_the_scroll_direction() {
    if !super::reader_handoff_tests::in_child(
        "app::reader_read_ahead_tests::encoded_bytes_are_fetched_beyond_the_decode_window_in_the_scroll_direction",
        "READER_FETCH_AHEAD_CHILD",
    ) {
        return;
    }
    let scroll = LiveReader::start(
        |dir| (0..16).map(|i| whole_page(dir, i, 512, 1000)).collect(),
        (6, 0.),
    );
    let ids = |pages: &[usize]| {
        pages
            .iter()
            .map(|i| format!("page-{i}"))
            .collect::<Vec<_>>()
    };
    // Reading downward from page 6: decode window 5..=8, then three fetched.
    let first = reads();
    for page in 9..=11 {
        assert!(
            first.contains(&format!("page-{page}")),
            "page {page} was not fetched ahead: {first:?}"
        );
    }
    assert!(!retained(&scroll).contains(&9), "fetched, not decoded");
    assert!(
        !first.contains(&"page-12".to_string()),
        "bounded: {first:?}"
    );
    // Reversing fetches the other way, and nothing is read twice.
    let mut at = scroll.page_top(6);
    for _ in 0..4 {
        at -= 800.;
        scroll.scroll_to(at);
    }
    let all = reads();
    for page in [1, 2] {
        assert!(all.contains(&format!("page-{page}")), "{all:?}");
    }
    let mut once = BTreeSet::new();
    let again = all
        .iter()
        .filter(|id| !once.insert((*id).clone()))
        .collect::<Vec<_>>();
    assert!(again.is_empty(), "read twice: {again:?} in {all:?}");
    let _ = ids(&[]);
}
