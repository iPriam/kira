//! UTC calendar arithmetic, enough to schedule releases on a weekday.
//!
//! Kept to `std` on purpose: the release schedule needs a civil date, a
//! weekday, and "the next Tuesday", which is small enough that a date crate
//! would cost the workspace a dependency for three functions.

use std::time::{SystemTime, UNIX_EPOCH};

/// A failure to read or convert a date.
#[derive(Debug, thiserror::Error)]
pub enum CalendarError {
    /// The system clock reads before 1970-01-01 UTC.
    #[error("the system clock reads before the Unix epoch")]
    ClockBeforeEpoch,
}

/// Days since 1970-01-01 UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DayNumber(i64);

/// A civil date in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilDate {
    /// The proleptic Gregorian year.
    pub year: i64,
    /// The month, 1..=12.
    pub month: u8,
    /// The day of the month, 1..=31.
    pub day: u8,
}

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    /// Sunday.
    Sunday,
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
}

impl Weekday {
    /// The weekday's English name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Sunday => "Sunday",
            Self::Monday => "Monday",
            Self::Tuesday => "Tuesday",
            Self::Wednesday => "Wednesday",
            Self::Thursday => "Thursday",
            Self::Friday => "Friday",
            Self::Saturday => "Saturday",
        }
    }

    /// The weekday's index with Sunday at 0, matching [`DayNumber::weekday`].
    fn index(self) -> i64 {
        match self {
            Self::Sunday => 0,
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
        }
    }
}

impl DayNumber {
    /// Today's day number in UTC.
    pub fn today() -> Result<Self, CalendarError> {
        let since_epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CalendarError::ClockBeforeEpoch)?;
        let days = i64::try_from(since_epoch.as_secs() / 86_400)
            .map_err(|_| CalendarError::ClockBeforeEpoch)?;
        Ok(Self(days))
    }

    /// The day number this many days later.
    pub fn plus_days(self, days: i64) -> Self {
        Self(self.0 + days)
    }

    /// Whole days from this day to a later one; zero when it is not later.
    pub fn days_until(self, other: Self) -> u32 {
        u32::try_from((other.0 - self.0).max(0)).unwrap_or(u32::MAX)
    }

    /// The weekday this day falls on. 1970-01-01 was a Thursday.
    pub fn weekday(self) -> Weekday {
        match (self.0 + 4).rem_euclid(7) {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            // rem_euclid(7) is 0..=6, so this arm is the remaining 6.
            _ => Weekday::Saturday,
        }
    }

    /// This day if it already falls on `weekday`, otherwise the next one that
    /// does.
    pub fn next_or_same(self, weekday: Weekday) -> Self {
        let ahead = (weekday.index() - self.weekday().index()).rem_euclid(7);
        Self(self.0 + ahead)
    }

    /// The civil date this day number names, by Howard Hinnant's algorithm.
    pub fn civil(self) -> CivilDate {
        let z = self.0 + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        CivilDate {
            year: year + i64::from(month <= 2),
            // `month` is 1..=12 and `day` is 1..=31 by construction.
            month: month as u8,
            day: day as u8,
        }
    }
}

impl CivilDate {
    /// The day number this date names, by Howard Hinnant's algorithm.
    ///
    /// The inverse of [`DayNumber::civil`]. Production code reads the clock
    /// and converts one way; naming a specific date is something only the
    /// tests do, which is what pins the conversion in both directions.
    #[cfg(test)]
    pub fn day_number(self) -> DayNumber {
        let month = i64::from(self.month);
        let year = self.year - i64::from(month <= 2);
        let era = if year >= 0 { year } else { year - 399 } / 400;
        let year_of_era = year - era * 400;
        let shifted_month = if month > 2 { month - 3 } else { month + 9 };
        let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(self.day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        DayNumber(era * 146_097 + day_of_era - 719_468)
    }
}

impl std::fmt::Display for CivilDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i64, month: u8, day: u8) -> CivilDate {
        CivilDate { year, month, day }
    }

    #[test]
    fn the_epoch_is_a_thursday() {
        assert_eq!(DayNumber(0).weekday(), Weekday::Thursday);
        assert_eq!(DayNumber(0).civil(), date(1970, 1, 1));
    }

    #[test]
    fn civil_and_day_number_round_trip() {
        for date in [
            date(1970, 1, 1),
            date(1999, 12, 31),
            date(2000, 2, 29),
            date(2026, 7, 30),
            date(2026, 8, 4),
            date(2027, 1, 1),
        ] {
            assert_eq!(date.day_number().civil(), date, "round trip for {date}");
        }
    }

    #[test]
    fn known_weekdays_match_the_calendar() {
        assert_eq!(date(2026, 7, 30).day_number().weekday(), Weekday::Thursday);
        assert_eq!(date(2026, 7, 28).day_number().weekday(), Weekday::Tuesday);
        assert_eq!(date(2026, 8, 4).day_number().weekday(), Weekday::Tuesday);
        assert_eq!(date(2026, 8, 1).day_number().weekday(), Weekday::Saturday);
    }

    #[test]
    fn next_or_same_keeps_a_day_that_already_matches() {
        let tuesday = date(2026, 8, 4).day_number();
        assert_eq!(tuesday.next_or_same(Weekday::Tuesday), tuesday);
    }

    #[test]
    fn next_or_same_crosses_the_month_boundary() {
        let thursday = date(2026, 7, 30).day_number();
        let next = thursday.next_or_same(Weekday::Tuesday);
        assert_eq!(next.civil(), date(2026, 8, 4));
        assert_eq!(thursday.days_until(next), 5);
    }

    #[test]
    fn days_until_an_earlier_day_is_zero() {
        let later = date(2026, 8, 4).day_number();
        let earlier = date(2026, 7, 30).day_number();
        assert_eq!(later.days_until(earlier), 0);
    }

    #[test]
    fn a_week_later_is_the_same_weekday() {
        let tuesday = date(2026, 8, 4).day_number();
        assert_eq!(tuesday.plus_days(7).civil(), date(2026, 8, 11));
        assert_eq!(tuesday.plus_days(7).weekday(), Weekday::Tuesday);
    }
}
