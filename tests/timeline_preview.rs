//! Opt-in software snapshots of Updates and History with realistic lines.
//! Covers are deterministic colored fixtures.
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use nova_reader::{Anim, AppWindow, Appearance, ShelfEntry, TimelineBridge, TimelineEntry};
use slint::{ComponentHandle, LogicalSize, ModelRc, VecModel};
use std::time::Duration;

fn settle() {
    for _ in 0..32 {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

fn cover(index: usize) -> slint::Image {
    let palette = [
        (59u8, 42u8, 107u8),
        (122, 46, 46),
        (28, 59, 47),
        (74, 29, 79),
        (15, 58, 74),
    ];
    let (r, g, b) = palette[index % palette.len()];
    let mut pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(120, 180);
    for (i, pixel) in pixels.make_mut_slice().iter_mut().enumerate() {
        let tint = ((i % 120 + i / 120) / 40 % 2 * 40) as u8;
        *pixel = slint::Rgba8Pixel::new(r.saturating_add(tint), g.saturating_add(tint), b, 255);
    }
    slint::Image::from_rgba8(pixels)
}

struct Lines {
    out: Vec<TimelineEntry>,
    y: f32,
    /// Lay cards out in two columns, as `timeline_groups::two_columns` does.
    wide: bool,
    /// Wide History: whole days side by side (`two_column_days`).
    by_day: bool,
}

impl Lines {
    fn two_column_days(&mut self) {
        let starts: Vec<usize> = (0..self.out.len())
            .filter(|&i| self.out[i].kind == 0)
            .collect();
        let original: Vec<f32> = self.out.iter().map(|e| e.y).collect();
        let mut columns = [original[0]; 2];
        for (k, &from) in starts.iter().enumerate() {
            let to = starts.get(k + 1).copied().unwrap_or(self.out.len());
            let next = starts.get(k + 1).map_or(self.y, |&n| original[n]);
            let column = usize::from(columns[1] < columns[0]);
            let shift = columns[column] - original[from];
            for entry in &mut self.out[from..to] {
                entry.y += shift;
                entry.column = column as i32 + 1;
            }
            columns[column] += next - original[from];
        }
        self.y = columns[0].max(columns[1]);
    }
    fn two_columns(&mut self) {
        let starts: Vec<usize> = (0..self.out.len())
            .filter(|&i| [0, 1, 5, 6].contains(&self.out[i].kind))
            .collect();
        let original: Vec<f32> = self.out.iter().map(|e| e.y).collect();
        let mut columns = [original[0]; 2];
        let mut y = original[0];
        for (k, &from) in starts.iter().enumerate() {
            let to = starts.get(k + 1).copied().unwrap_or(self.out.len());
            let next = starts.get(k + 1).map_or(self.y, |&n| original[n]);
            let size = next - original[from];
            if self.out[from].kind == 0 {
                y = columns[0].max(columns[1]);
                self.out[from].y = y;
                y += size;
                columns = [y; 2];
                continue;
            }
            let column = usize::from(columns[1] < columns[0]);
            let shift = columns[column] - original[from];
            for entry in &mut self.out[from..to] {
                entry.y += shift;
                entry.column = column as i32 + 1;
            }
            columns[column] += size;
        }
        self.y = columns[0].max(columns[1]).max(y);
    }
    fn push(&mut self, mut entry: TimelineEntry, height: f32, last: bool) {
        entry.index = self.out.len() as i32;
        entry.y = self.y;
        entry.height = height;
        entry.last = last;
        if entry.progress == 0. {
            entry.progress = -1.;
        }
        self.y += height + if last { 8. } else { 0. };
        self.out.push(entry);
    }
    fn finish(mut self, bridge: &TimelineBridge) {
        if self.wide && self.by_day {
            self.two_column_days();
        } else if self.wide {
            self.two_columns();
        }
        let ends: Vec<usize> = (0..self.out.len())
            .filter(|&i| self.out[i].kind == 0)
            .collect();
        for (k, &i) in ends.iter().enumerate() {
            let next = ends.get(k + 1).map_or(self.y, |&j| self.out[j].y);
            let to = ends.get(k + 1).copied().unwrap_or(self.out.len());
            self.out[i].span = if self.out[i].column == 0 {
                next - self.out[i].y
            } else {
                self.out[i..to]
                    .iter()
                    .map(|e| e.y + e.height)
                    .fold(0., f32::max)
                    - self.out[i].y
                    + 8.
            };
        }
        let days: Vec<_> = ends.iter().map(|&i| self.out[i].clone()).collect();
        bridge.set_content_height(self.y);
        bridge.set_days(ModelRc::new(VecModel::from(days)));
        bridge.set_entries(ModelRc::new(VecModel::from(self.out)));
    }
}

fn line(kind: i32) -> TimelineEntry {
    TimelineEntry {
        kind,
        progress: -1.,
        ..Default::default()
    }
}

fn updates(bridge: &TimelineBridge, wide: bool) {
    let mut l = Lines {
        out: Vec::new(),
        y: 0.,
        wide,
        by_day: false,
    };
    l.push(
        TimelineEntry {
            title: "Today".into(),
            subtitle: "Sat 8 Nov · 18 chapters".into(),
            has_unread: true,
            ..line(0)
        },
        44.,
        false,
    );
    l.push(
        TimelineEntry {
            title: "Salt & Saffron".into(),
            emphasis: "14 new".into(),
            subtitle: "Chapters 41–54".into(),
            unread: 14,
            count: 14,
            poster: cover(1),
            ..line(1)
        },
        116.,
        false,
    );
    for (n, read, state) in [(54, false, 0), (53, false, 2), (52, true, 5)] {
        l.push(
            TimelineEntry {
                subtitle: format!("Chapter {n}").into(),
                read,
                download_state: state,
                download_progress: 0.4,
                ..line(2)
            },
            48.,
            false,
        );
    }
    l.push(
        TimelineEntry {
            count: 11,
            ..line(3)
        },
        42.,
        false,
    );
    l.push(
        TimelineEntry {
            action: "Read 41".into(),
            ..line(4)
        },
        50.,
        true,
    );
    l.push(
        TimelineEntry {
            title: "Moonlit Cartographer".into(),
            subtitle: "Chapter 88: The Glass Lighthouse".into(),
            poster: cover(0),
            ..line(5)
        },
        116.,
        true,
    );
    l.push(
        TimelineEntry {
            title: "Tidebound".into(),
            emphasis: "2 new".into(),
            subtitle: "Chapters 203–204".into(),
            unread: 2,
            count: 2,
            expanded: true,
            poster: cover(4),
            ..line(1)
        },
        116.,
        false,
    );
    l.push(
        TimelineEntry {
            subtitle: "Chapter 204".into(),
            download_state: 5,
            ..line(2)
        },
        48.,
        false,
    );
    l.push(
        TimelineEntry {
            subtitle: "Chapter 203".into(),
            ..line(2)
        },
        48.,
        false,
    );
    l.push(
        TimelineEntry {
            action: "Read 203".into(),
            ..line(4)
        },
        50.,
        true,
    );
    l.push(
        TimelineEntry {
            title: "Yesterday".into(),
            subtitle: "Fri 7 Nov · 7 chapters".into(),
            has_unread: true,
            ..line(0)
        },
        44.,
        false,
    );
    for (i, name) in [
        "The Ninth Lantern",
        "Paper Tigers of Okane and the Lantern Festival of the Northern Coast",
        "Ember Courier",
    ]
    .iter()
    .enumerate()
    {
        l.push(
            TimelineEntry {
                title: (*name).into(),
                subtitle: "Chapter 71".into(),
                read: i == 1,
                poster: cover(i + 2),
                ..line(5)
            },
            116.,
            true,
        );
    }
    l.finish(bridge);
    bridge.set_count_label("17 new".into());
    bridge.set_status_line("Checked Today 14:02 · 46 titles".into());
    bridge.set_filter_counts(ModelRc::new(VecModel::from(vec![25, 17, 3])));
}

fn history(bridge: &TimelineBridge, timeline: bool, wide: bool) {
    let mut l = Lines {
        out: Vec::new(),
        y: 312.,
        wide,
        by_day: true,
    };
    if timeline {
        l.push(
            TimelineEntry {
                title: "Today".into(),
                subtitle: "Sat 8 Nov".into(),
                ..line(0)
            },
            36.,
            false,
        );
        l.push(
            TimelineEntry {
                title: "Moonlit Cartographer".into(),
                emphasis: "Read 6".into(),
                subtitle: "Chapters 82–87".into(),
                time: "21:42".into(),
                poster: cover(0),
                ..line(6)
            },
            82.,
            false,
        );
        l.push(
            TimelineEntry {
                title: "Paper Tigers of Okane and the Lantern Festival of the Northern Coast"
                    .into(),
                subtitle: "Chapter 99 · page 12/44".into(),
                time: "13:10".into(),
                poster: cover(3),
                ..line(6)
            },
            82.,
            false,
        );
    } else {
        l.push(
            TimelineEntry {
                title: "Today".into(),
                subtitle: "Sat 8 Nov · 8 chapters".into(),
                ..line(0)
            },
            44.,
            false,
        );
        l.push(
            TimelineEntry {
                title: "Moonlit Cartographer".into(),
                emphasis: "6 chapters".into(),
                subtitle: "Chapters 82–87 · 20:04–21:42".into(),
                progress: 0.6,
                expanded: true,
                poster: cover(0),
                ..line(1)
            },
            116.,
            false,
        );
        for (n, t) in [(87, "21:42 · page 18 of 30"), (86, "21:20"), (85, "21:02")] {
            l.push(
                TimelineEntry {
                    subtitle: format!("Chapter {n}").into(),
                    time: t.into(),
                    ..line(2)
                },
                48.,
                false,
            );
        }
        l.push(
            TimelineEntry {
                action: "Resume Ch. 87".into(),
                ..line(4)
            },
            50.,
            true,
        );
        l.push(
            TimelineEntry {
                title: "Paper Tigers of Okane and the Lantern Festival of the Northern Coast"
                    .into(),
                subtitle: "Chapter 99".into(),
                time: "13:10".into(),
                progress: 0.27,
                poster: cover(3),
                ..line(5)
            },
            116.,
            true,
        );
    }
    let timeline_view = timeline;
    l.push(
        TimelineEntry {
            title: "Yesterday".into(),
            subtitle: if timeline_view {
                "Fri 7 Nov"
            } else {
                "Fri 7 Nov · 3 chapters"
            }
            .into(),
            ..line(0)
        },
        if timeline_view { 36. } else { 44. },
        false,
    );
    for (i, (name, detail, time)) in [
        ("Ember Courier", "Chapter 18", "22:15"),
        ("Salt & Saffron", "Chapter 40", "19:02"),
        ("Tidebound", "Chapter 202 · page 3/41", "08:40"),
    ]
    .iter()
    .enumerate()
    {
        l.push(
            TimelineEntry {
                title: (*name).into(),
                subtitle: (*detail).into(),
                time: (*time).into(),
                poster: cover(i + 2),
                ..line(if timeline_view { 6 } else { 5 })
            },
            if timeline_view { 82. } else { 116. },
            !timeline_view,
        );
    }
    l.finish(bridge);
    bridge.set_list_top(312.);
    bridge.set_shelf(ModelRc::new(VecModel::from(
        [
            ("Moonlit Cartographer", "Ch. 87", "Page 18 of 30", 0.6),
            ("Ember Courier", "Next: Ch. 19", "Ready", 0.),
            ("Salt & Saffron", "Next: Ch. 41", "Ready", 0.),
        ]
        .iter()
        .enumerate()
        .map(|(i, (title, headline, detail, progress))| ShelfEntry {
            title: (*title).into(),
            headline: (*headline).into(),
            detail: (*detail).into(),
            progress: *progress,
            poster: cover(i),
            ..Default::default()
        })
        .collect::<Vec<_>>(),
    )));
}

#[test]
#[ignore = "manual production UI previews; writes PNGs to NOVA_READER_PREVIEW_DIR"]
fn updates_and_history_previews() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .unwrap();
    let dir = std::path::PathBuf::from(
        std::env::var("NOVA_READER_PREVIEW_DIR")
            .unwrap_or_else(|_| "target/timeline-preview".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    for (width, height) in [(390u32, 844u32), (1280, 800)] {
        for (name, section, view) in [
            ("updates", 1, 0),
            ("history-cards", 2, 0),
            ("history-timeline", 2, 1),
        ] {
            let ui = AppWindow::new().unwrap();
            ui.global::<Anim>().set_enabled(false);
            ui.global::<Appearance>().set_history_view(view);
            let bridge = ui.global::<TimelineBridge>();
            let wide = width >= 960;
            bridge.set_columns(if wide { 2 } else { 1 });
            if section == 1 {
                updates(&bridge, wide)
            } else {
                history(&bridge, view == 1, wide)
            }
            ui.set_section(section);
            ui.show().unwrap();
            ui.window()
                .set_size(LogicalSize::new(width as f32, height as f32));
            settle();
            let pixels = ui.window().take_snapshot().unwrap();
            let path = dir.join(format!("{name}-{width}.png"));
            image::save_buffer(
                &path,
                pixels.as_bytes(),
                pixels.width(),
                pixels.height(),
                image::ColorType::Rgba8,
            )
            .unwrap();
            eprintln!("preview: {}", path.display());
        }
    }
}
