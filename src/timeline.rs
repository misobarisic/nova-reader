//! Civil dates and local reading times for History and Updates. The OS may
//! decline local-offset queries; UTC keeps the timeline usable in that case.
use nova_reader_ui::backend_text as text;
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, OffsetDateTime, UtcOffset};

/// Provider date-only values describe a civil day, not UTC midnight. Noon in
/// the device's locale keeps that date intact when it becomes a timeline row.
pub(crate) fn parse_source_date(value: Option<&str>) -> Option<i64> {
    let value = value?;
    if value.len() > 128 {
        return None;
    }
    let value = value.trim();
    if let Ok(datetime) = OffsetDateTime::parse(value, &Rfc3339) {
        let unix = datetime.unix_timestamp();
        return OffsetDateTime::from_unix_timestamp(unix).ok().map(|_| unix);
    }
    // Limit date-only input to the provider contract's YYYY-MM-DD shape;
    // calendar/leap-year validation belongs to time, not custom date math.
    if value.len() != 10
        || !value.bytes().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
    {
        return None;
    }
    let noon = Date::parse(value, &Iso8601::DATE)
        .ok()?
        .with_hms(12, 0, 0)
        .ok()?;
    let offset = UtcOffset::local_offset_at(noon.assume_utc()).unwrap_or(UtcOffset::UTC);
    let unix = noon.assume_offset(offset).unix_timestamp();
    OffsetDateTime::from_unix_timestamp(unix).ok().map(|_| unix)
}

pub struct TimelineStamp {
    pub group: String,
    pub timestamp: String,
}

pub struct TimelineClock {
    now: OffsetDateTime,
    offset: UtcOffset,
}

impl TimelineClock {
    pub fn now() -> Self {
        let now = OffsetDateTime::now_utc();
        let offset = UtcOffset::local_offset_at(now).unwrap_or(UtcOffset::UTC);
        Self { now, offset }
    }

    pub fn history(&self, unix: i64) -> TimelineStamp {
        self.stamp(Some(unix))
    }

    pub fn release(&self, unix: Option<i64>) -> TimelineStamp {
        self.stamp(unix)
    }

    /// Absolute upload date for chapter metadata; omit absent/invalid dates.
    pub fn release_date(&self, unix: Option<i64>) -> String {
        self.local(unix)
            .map(|local| date_label(local.date()))
            .unwrap_or_default()
    }

    fn local(&self, unix: Option<i64>) -> Option<OffsetDateTime> {
        let utc = OffsetDateTime::from_unix_timestamp(unix?).ok()?;
        // Query the event's offset as well, so older entries observe DST.
        let offset = UtcOffset::local_offset_at(utc).unwrap_or(self.offset);
        Some(utc.checked_to_offset(offset).unwrap_or(utc))
    }

    fn stamp(&self, unix: Option<i64>) -> TimelineStamp {
        let Some(local) = self.local(unix) else {
            return TimelineStamp {
                group: text::tr("Unknown date"),
                timestamp: String::new(),
            };
        };
        stamp_at(
            local,
            self.now
                .checked_to_offset(self.offset)
                .unwrap_or(self.now)
                .date(),
        )
    }
}

fn stamp_at(local: OffsetDateTime, today: Date) -> TimelineStamp {
    let date = local.date();
    let group = if date == today {
        text::tr("Today")
    } else if Some(date) == today.previous_day() {
        text::tr("Yesterday")
    } else {
        date_label(date)
    };
    TimelineStamp {
        group,
        timestamp: format!("{:02}:{:02}", local.hour(), local.minute()),
    }
}

fn date_label(date: Date) -> String {
    format!("{} {}, {}", date.month(), date.day(), date.year())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::{Month, Time};

    #[test]
    fn yesterday_crosses_a_year_boundary() {
        let today = Date::from_calendar_date(2026, Month::January, 1).unwrap();
        let yesterday = today.previous_day().unwrap();
        let stamp = stamp_at(yesterday.with_time(Time::MIDNIGHT).assume_utc(), today);
        assert_eq!(stamp.group, "Yesterday");
        assert_eq!(stamp.timestamp, "00:00");
    }

    #[test]
    fn local_day_and_time_follow_the_offset() {
        let date = Date::from_calendar_date(2026, Month::October, 31).unwrap();
        let utc = date.with_hms(23, 45, 0).unwrap().assume_utc();
        let local = utc.to_offset(UtcOffset::from_hms(2, 0, 0).unwrap());
        let stamp = stamp_at(local, local.date());
        assert_eq!(stamp.group, "Today");
        assert_eq!(stamp.timestamp, "01:45");
        let older = stamp_at(utc, date.next_day().unwrap().next_day().unwrap());
        assert_eq!(older.group, "October 31, 2026");
    }

    #[test]
    fn missing_or_invalid_dates_share_the_unknown_group() {
        let clock = TimelineClock {
            now: OffsetDateTime::UNIX_EPOCH,
            offset: UtcOffset::UTC,
        };
        for unix in [None, Some(i64::MAX)] {
            let stamp = clock.release(unix);
            assert_eq!(stamp.group, "Unknown date");
            assert!(stamp.timestamp.is_empty());
        }
    }

    #[test]
    fn chapter_upload_date_is_absolute_and_missing_dates_are_omitted() {
        let unix = parse_source_date(Some("2026-10-09")).unwrap();
        let now = OffsetDateTime::from_unix_timestamp(unix).unwrap();
        let clock = TimelineClock {
            now,
            offset: UtcOffset::UTC,
        };
        assert_eq!(
            clock.release_date(Some(now.unix_timestamp())),
            "October 9, 2026"
        );
        assert_eq!(clock.release_date(None), "");
        assert_eq!(clock.release_date(Some(i64::MAX)), "");
    }

    #[test]
    fn source_date_parser_validates_calendar_and_datetime_formats() {
        assert!(parse_source_date(Some("2024-02-29")).is_some());
        assert!(parse_source_date(Some("2025-02-29")).is_none());
        assert!(parse_source_date(Some("2026-10-09junk")).is_none());
        assert!(parse_source_date(Some("2026-10-09T12:00:00")).is_none());
        assert!(parse_source_date(Some(&"0".repeat(129))).is_none());
        assert_eq!(
            parse_source_date(Some("2026-10-09T00:30:00+02:00")),
            parse_source_date(Some("2026-10-08T22:30:00Z"))
        );
    }
}
