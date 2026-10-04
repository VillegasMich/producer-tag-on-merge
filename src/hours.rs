//! `QUIET_HOURS` parsing and matching. The only place where `TIMEZONE` is used.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, NaiveTime, Utc};
use chrono_tz::Tz;

/// A daily time range `HH:MM-HH:MM` in local time; may cross midnight (`22:00-08:00`).
/// The start is inclusive, the end exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    start: NaiveTime,
    end: NaiveTime,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum QuietHoursError {
    #[error("expected HH:MM-HH:MM, e.g. 22:00-08:00")]
    Format,
    #[error("start and end are equal; leave QUIET_HOURS unset to disable it")]
    Empty,
}

impl QuietHours {
    /// Whether `now` falls inside the range, read as wall-clock time in `tz`.
    pub fn contains(&self, now: DateTime<Utc>, tz: Tz) -> bool {
        let t = now.with_timezone(&tz).time();
        if self.start < self.end {
            self.start <= t && t < self.end
        } else {
            t >= self.start || t < self.end
        }
    }
}

impl FromStr for QuietHours {
    type Err = QuietHoursError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (start, end) = s.trim().split_once('-').ok_or(QuietHoursError::Format)?;
        let parse = |t: &str| {
            NaiveTime::parse_from_str(t.trim(), "%H:%M").map_err(|_| QuietHoursError::Format)
        };
        let (start, end) = (parse(start)?, parse(end)?);
        if start == end {
            return Err(QuietHoursError::Empty);
        }
        Ok(Self { start, end })
    }
}

impl fmt::Display for QuietHours {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}",
            self.start.format("%H:%M"),
            self.end.format("%H:%M")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339).unwrap().to_utc()
    }

    #[test]
    fn parses_and_displays() {
        let q: QuietHours = "22:00-08:00".parse().unwrap();
        assert_eq!(q.to_string(), "22:00-08:00");
        let q: QuietHours = " 9:30 - 17:00 ".parse().unwrap();
        assert_eq!(q.to_string(), "09:30-17:00");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!("22:00".parse::<QuietHours>(), Err(QuietHoursError::Format));
        assert_eq!(
            "25:00-08:00".parse::<QuietHours>(),
            Err(QuietHoursError::Format)
        );
        assert_eq!("ab-cd".parse::<QuietHours>(), Err(QuietHoursError::Format));
        assert_eq!(
            "08:00-08:00".parse::<QuietHours>(),
            Err(QuietHoursError::Empty)
        );
    }

    #[test]
    fn same_day_range() {
        let q: QuietHours = "12:00-13:00".parse().unwrap();
        assert!(!q.contains(at("2026-10-04T11:59:59Z"), Tz::UTC));
        assert!(q.contains(at("2026-10-04T12:00:00Z"), Tz::UTC));
        assert!(q.contains(at("2026-10-04T12:59:59Z"), Tz::UTC));
        assert!(!q.contains(at("2026-10-04T13:00:00Z"), Tz::UTC));
    }

    #[test]
    fn range_crossing_midnight() {
        let q: QuietHours = "22:00-08:00".parse().unwrap();
        assert!(q.contains(at("2026-10-04T23:30:00Z"), Tz::UTC));
        assert!(q.contains(at("2026-10-04T03:00:00Z"), Tz::UTC));
        assert!(!q.contains(at("2026-10-04T08:00:00Z"), Tz::UTC));
        assert!(!q.contains(at("2026-10-04T15:00:00Z"), Tz::UTC));
    }

    #[test]
    fn matches_in_the_configured_timezone() {
        // Bogotá is UTC-5: 02:00 UTC is 21:00 local (outside), 04:00 UTC is 23:00 (inside).
        let q: QuietHours = "22:00-08:00".parse().unwrap();
        let bogota: Tz = "America/Bogota".parse().unwrap();
        assert!(!q.contains(at("2026-10-04T02:00:00Z"), bogota));
        assert!(q.contains(at("2026-10-04T04:00:00Z"), bogota));
    }
}
