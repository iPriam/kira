//! The Kira version scheme, and the release window it implies.
//!
//! A version is `<year>.<month>.<week>[.<increment>]`:
//!
//! - `year` counts from the version epoch, so 2026 is `1`.
//! - `month` is the calendar month, so August 2026 is `1.8`. It is not a
//!   semantic minor: it rolls over on the first of every month whether or
//!   not anything shipped.
//! - `week` is the zero-based week of that month — days 1-7 are week `0`,
//!   days 8-14 week `1`, and so on. A month's first release is always `.0`.
//! - `increment` disambiguates a second release inside one week, and is
//!   omitted for the first. `1.8.0` is the week's first release, `1.8.0.1`
//!   its second.
//!
//! The first three fields come from the date alone, so nothing depends on
//! the release landing on a particular weekday; the usual cadence is Tuesday
//! and the report names the next one. The increment is the one field read
//! from the tags, and it exists because two releases in one week would
//! otherwise compute the same version.
//!
//! Cargo cannot hold a four-component version — `version = "1.8.0.1"` is
//! rejected outright as `unexpected character '.' after patch version
//! number`. [`Version::cargo_version`] is the three-component prefix that
//! belongs in `Cargo.toml`, and the report prints it whenever the two differ.

use crate::calendar::{CalendarError, CivilDate, DayNumber, Weekday};

/// The year that the major field counts from: 2026 is major `1`.
const VERSION_EPOCH_YEAR: i64 = 2025;

/// The weekday the usual cadence ships on.
const CADENCE_DAY: Weekday = Weekday::Tuesday;

/// A failure to compute the release window.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    /// The date could not be read.
    #[error(transparent)]
    Calendar(#[from] CalendarError),
    /// The date falls before the version epoch, so it has no major number.
    #[error("{year} is before the version epoch {VERSION_EPOCH_YEAR}")]
    YearBeforeEpoch {
        /// The year that could not be numbered.
        year: i64,
    },
    /// The repository's tags could not be listed.
    #[error("could not list git tags: {reason}")]
    TagsUnavailable {
        /// What went wrong.
        reason: String,
    },
}

/// A Kira release version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    /// Years since the version epoch.
    pub major: u32,
    /// The calendar month, 1..=12.
    pub month: u8,
    /// The zero-based week of the month.
    pub week: u32,
    /// Which release of that week this is; zero for the first, and omitted
    /// from the printed form.
    pub increment: u32,
}

impl Version {
    /// The version a release on this date carries, given the tags that exist.
    ///
    /// The date fixes year, month, and week; the tags fix only the increment,
    /// which is one past the highest already tagged for that same week.
    pub fn for_date(date: CivilDate, tags: &[Version]) -> Result<Self, ReleaseError> {
        let major = u32::try_from(date.year - VERSION_EPOCH_YEAR)
            .map_err(|_| ReleaseError::YearBeforeEpoch { year: date.year })?;
        let week = u32::from(date.day.saturating_sub(1) / 7);
        let highest = tags
            .iter()
            .filter(|tag| tag.major == major && tag.month == date.month && tag.week == week)
            .map(|tag| tag.increment)
            .max();
        Ok(Self {
            major,
            month: date.month,
            week,
            increment: highest.map_or(0, |highest| highest + 1),
        })
    }

    /// The three-component prefix, which is what `Cargo.toml` can hold.
    ///
    /// Equal to the printed form for a week's first release, and the printed
    /// form minus its increment for any later one.
    pub fn cargo_version(self) -> String {
        format!("{}.{}.{}", self.major, self.month, self.week)
    }

    /// Whether this version needs an increment Cargo cannot express.
    pub fn exceeds_cargo(self) -> bool {
        self.increment > 0
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.month, self.week)?;
        if self.increment > 0 {
            write!(f, ".{}", self.increment)?;
        }
        Ok(())
    }
}

/// Parse a `v<major>.<month>.<week>[.<increment>]` tag; `None` for anything
/// else, which covers the LLVM bundle tags and any branch-shaped ref.
pub fn parse_tag(tag: &str) -> Option<Version> {
    let mut parts = tag.strip_prefix('v')?.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let month = parts.next()?.parse::<u8>().ok()?;
    let week = parts.next()?.parse::<u32>().ok()?;
    let increment = match parts.next() {
        Some(text) => text.parse::<u32>().ok()?,
        None => 0,
    };
    if parts.next().is_some() || month == 0 || month > 12 {
        return None;
    }
    Some(Version {
        major,
        month,
        week,
        increment,
    })
}

/// Every version tag in a newline-separated `git tag --list` output.
pub fn parse_tags(listing: &str) -> Vec<Version> {
    listing
        .lines()
        .filter_map(|line| parse_tag(line.trim()))
        .collect()
}

/// A version and the day it ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// The version this day carries.
    pub version: Version,
    /// The day itself.
    pub date: CivilDate,
    /// Whole days from today; zero for today.
    pub days_away: u32,
}

/// What ships today, and what the next cadence day carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseWindow {
    /// Today's weekday.
    pub today_weekday: Weekday,
    /// Shipping today, at today's version.
    pub current: Slot,
    /// The next Tuesday, at that day's version.
    pub next: Slot,
}

impl ReleaseWindow {
    /// Whether today is the usual cadence day.
    pub fn on_cadence_today(&self) -> bool {
        self.today_weekday == CADENCE_DAY
    }

    /// Compute the window for a given day against a set of existing tags.
    pub fn compute(today: DayNumber, tags: &[Version]) -> Result<Self, ReleaseError> {
        // The next cadence day is always a later day: shipping today is what
        // the first line covers, so the second line is never a repeat of it.
        let next_cadence = today.plus_days(1).next_or_same(CADENCE_DAY);
        Ok(Self {
            today_weekday: today.weekday(),
            current: slot(today, today, tags)?,
            next: slot(next_cadence, today, tags)?,
        })
    }

    /// Compute the window for today against this repository's tags.
    pub fn for_today() -> Result<Self, ReleaseError> {
        Self::compute(DayNumber::today()?, &parse_tags(&git_tags()?))
    }

    /// The report `devflow release-window` prints.
    pub fn report(&self) -> String {
        let cadence = if self.on_cadence_today() {
            "on cadence"
        } else {
            "off cadence"
        };
        let mut out = format!(
            "today     {} {} — {cadence}\n",
            self.current.date,
            self.today_weekday.label()
        );
        out.push_str(&format!(
            "current   {}  ships today{}\n",
            self.current.version,
            cargo_note(self.current.version)
        ));
        out.push_str(&format!(
            "next      {}  ships {} {} — in {}{}\n",
            self.next.version,
            CADENCE_DAY.label(),
            self.next.date,
            days_phrase(self.next.days_away),
            cargo_note(self.next.version)
        ));
        out
    }
}

/// One line's worth of the window.
fn slot(day: DayNumber, today: DayNumber, tags: &[Version]) -> Result<Slot, ReleaseError> {
    let date = day.civil();
    Ok(Slot {
        version: Version::for_date(date, tags)?,
        date,
        days_away: today.days_until(day),
    })
}

/// What `Cargo.toml` must say, named only when it differs from the version.
fn cargo_note(version: Version) -> String {
    if version.exceeds_cargo() {
        format!("  (Cargo.toml: {})", version.cargo_version())
    } else {
        String::new()
    }
}

/// `1 day` / `6 days`, so the report reads as a sentence.
fn days_phrase(days: u32) -> String {
    if days == 1 {
        String::from("1 day")
    } else {
        format!("{days} days")
    }
}

/// This repository's tags, as `git tag --list` prints them.
fn git_tags() -> Result<String, ReleaseError> {
    let output = std::process::Command::new("git")
        .args(["tag", "--list"])
        .output()
        .map_err(|error| ReleaseError::TagsUnavailable {
            reason: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(ReleaseError::TagsUnavailable {
            reason: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    String::from_utf8(output.stdout).map_err(|error| ReleaseError::TagsUnavailable {
        reason: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i64, month: u8, day: u8) -> DayNumber {
        CivilDate { year, month, day }.day_number()
    }

    fn date(year: i64, month: u8, day: u8) -> CivilDate {
        CivilDate { year, month, day }
    }

    fn version(major: u32, month: u8, week: u32) -> Version {
        Version {
            major,
            month,
            week,
            increment: 0,
        }
    }

    fn incremented(major: u32, month: u8, week: u32, increment: u32) -> Version {
        Version {
            major,
            month,
            week,
            increment,
        }
    }

    fn untagged(date: CivilDate) -> Version {
        Version::for_date(date, &[]).expect("version")
    }

    #[test]
    fn the_first_seven_days_of_a_month_are_week_zero() {
        for day in 1..=7 {
            assert_eq!(untagged(date(2026, 8, day)), version(1, 8, 0), "day {day}");
        }
    }

    #[test]
    fn the_week_advances_every_seven_days() {
        assert_eq!(untagged(date(2026, 8, 8)), version(1, 8, 1));
        assert_eq!(untagged(date(2026, 8, 14)), version(1, 8, 1));
        assert_eq!(untagged(date(2026, 8, 15)), version(1, 8, 2));
        assert_eq!(untagged(date(2026, 8, 29)), version(1, 8, 4));
    }

    #[test]
    fn august_the_fourth_is_one_eight_zero() {
        assert_eq!(untagged(date(2026, 8, 4)).to_string(), "1.8.0");
    }

    #[test]
    fn a_new_month_returns_to_week_zero() {
        assert_eq!(untagged(date(2026, 9, 1)), version(1, 9, 0));
    }

    #[test]
    fn the_year_rolls_the_major_over() {
        assert_eq!(untagged(date(2027, 1, 5)), version(2, 1, 0));
    }

    #[test]
    fn a_year_before_the_epoch_has_no_major() {
        assert!(matches!(
            Version::for_date(date(2020, 1, 7), &[]),
            Err(ReleaseError::YearBeforeEpoch { year: 2020 })
        ));
    }

    #[test]
    fn the_first_release_of_a_week_carries_no_increment() {
        let computed = untagged(date(2026, 8, 4));
        assert_eq!(computed.increment, 0);
        assert_eq!(computed.to_string(), "1.8.0");
    }

    #[test]
    fn a_second_release_in_one_week_increments() {
        let tags = [version(1, 8, 0)];
        // Thursday of the same week as Tuesday the 4th.
        let computed = Version::for_date(date(2026, 8, 6), &tags).expect("version");
        assert_eq!(computed, incremented(1, 8, 0, 1));
        assert_eq!(computed.to_string(), "1.8.0.1");
    }

    #[test]
    fn increments_keep_stacking_within_the_week() {
        let tags = [version(1, 8, 0), incremented(1, 8, 0, 1)];
        let computed = Version::for_date(date(2026, 8, 7), &tags).expect("version");
        assert_eq!(computed.to_string(), "1.8.0.2");
    }

    #[test]
    fn an_increment_in_one_week_does_not_leak_into_the_next() {
        let tags = [version(1, 8, 0), incremented(1, 8, 0, 1)];
        let computed = Version::for_date(date(2026, 8, 11), &tags).expect("version");
        assert_eq!(computed.to_string(), "1.8.1");
    }

    #[test]
    fn an_increment_in_one_month_does_not_leak_into_the_next() {
        let tags = [version(1, 8, 0), incremented(1, 8, 0, 1)];
        let computed = Version::for_date(date(2026, 9, 1), &tags).expect("version");
        assert_eq!(computed.to_string(), "1.9.0");
    }

    #[test]
    fn a_computed_version_is_never_one_that_is_already_tagged() {
        let mut tags = vec![];
        // Ten releases on the same day must produce ten distinct versions.
        for _ in 0..10 {
            let computed = Version::for_date(date(2026, 8, 4), &tags).expect("version");
            assert!(!tags.contains(&computed), "{computed} was already tagged");
            tags.push(computed);
        }
        assert_eq!(tags.len(), 10);
    }

    #[test]
    fn cargo_gets_the_three_component_prefix() {
        assert_eq!(version(1, 8, 0).cargo_version(), "1.8.0");
        assert_eq!(incremented(1, 8, 0, 3).cargo_version(), "1.8.0");
        assert!(!version(1, 8, 0).exceeds_cargo());
        assert!(incremented(1, 8, 0, 1).exceeds_cargo());
    }

    #[test]
    fn an_off_cadence_day_still_names_its_own_version() {
        let window = ReleaseWindow::compute(day(2026, 7, 30), &[]).expect("window");
        assert!(!window.on_cadence_today());
        assert_eq!(window.today_weekday, Weekday::Thursday);
        assert_eq!(window.current.version, version(1, 7, 4));
        assert_eq!(window.current.days_away, 0);
    }

    #[test]
    fn the_next_line_is_the_coming_tuesday() {
        let window = ReleaseWindow::compute(day(2026, 7, 30), &[]).expect("window");
        assert_eq!(window.next.date, date(2026, 8, 4));
        assert_eq!(window.next.version, version(1, 8, 0));
        assert_eq!(window.next.days_away, 5);
    }

    #[test]
    fn a_tuesday_looks_a_week_ahead_rather_than_at_itself() {
        let window = ReleaseWindow::compute(day(2026, 8, 4), &[]).expect("window");
        assert!(window.on_cadence_today());
        assert_eq!(window.current.version, version(1, 8, 0));
        assert_eq!(window.next.date, date(2026, 8, 11));
        assert_eq!(window.next.version, version(1, 8, 1));
        assert_eq!(window.next.days_away, 7);
    }

    #[test]
    fn the_report_names_the_cargo_version_only_when_it_differs() {
        let plain = ReleaseWindow::compute(day(2026, 8, 4), &[]).expect("window");
        assert!(!plain.report().contains("Cargo.toml"), "{}", plain.report());

        let tags = [version(1, 8, 0)];
        let bumped = ReleaseWindow::compute(day(2026, 8, 6), &tags).expect("window");
        let report = bumped.report();
        assert!(
            report.contains("current   1.8.0.1  ships today  (Cargo.toml: 1.8.0)"),
            "{report}"
        );
    }

    #[test]
    fn only_version_shaped_tags_are_read() {
        let listing =
            "llvm-v22.1.4-kira.1\nv1.7.5\nbackup-2026-07\nv1.8\nv1.13.0\nv1.8.0.1\nv1.2.3.4.5\n";
        assert_eq!(
            parse_tags(listing),
            vec![version(1, 7, 5), incremented(1, 8, 0, 1)]
        );
    }

    #[test]
    fn a_three_component_tag_reads_as_increment_zero() {
        assert_eq!(parse_tag("v1.8.0"), Some(version(1, 8, 0)));
        assert_eq!(parse_tag("v1.8.0.0"), Some(version(1, 8, 0)));
    }

    #[test]
    fn the_report_reads_as_two_dated_lines() {
        let window = ReleaseWindow::compute(day(2026, 7, 30), &[]).expect("window");
        let report = window.report();
        assert!(
            report.contains("today     2026-07-30 Thursday — off cadence"),
            "{report}"
        );
        assert!(report.contains("current   1.7.4  ships today"), "{report}");
        assert!(
            report.contains("next      1.8.0  ships Tuesday 2026-08-04 — in 5 days"),
            "{report}"
        );
    }

    #[test]
    fn a_monday_is_one_day_from_cadence() {
        let window = ReleaseWindow::compute(day(2026, 8, 3), &[]).expect("window");
        assert_eq!(window.next.days_away, 1);
        assert!(window.report().contains("in 1 day"), "{}", window.report());
    }

    /// Every Tuesday of a month, in order.
    fn cadence_days(year: i64, month: u8) -> Vec<CivilDate> {
        let mut days = Vec::new();
        let mut cursor = date(year, month, 1).day_number().next_or_same(CADENCE_DAY);
        while cursor.civil().year == year && cursor.civil().month == month {
            days.push(cursor.civil());
            cursor = cursor.plus_days(7);
        }
        days
    }

    #[test]
    fn month_length_never_breaks_the_cadence_numbering() {
        for year in 2026..=2040 {
            for month in 1..=12u8 {
                let weeks: Vec<u32> = cadence_days(year, month)
                    .into_iter()
                    .map(|date| untagged(date).week)
                    .collect();
                let consecutive_from_zero: Vec<u32> =
                    (0..u32::try_from(weeks.len()).expect("count")).collect();
                assert_eq!(weeks, consecutive_from_zero, "{year}-{month:02}");
                assert!(
                    weeks.len() == 4 || weeks.len() == 5,
                    "{year}-{month:02} has {} cadence days",
                    weeks.len()
                );
            }
        }
    }

    #[test]
    fn no_day_of_any_month_reaches_week_five() {
        for year in 2026..=2040 {
            for month in 1..=12u8 {
                let mut cursor = date(year, month, 1).day_number();
                while cursor.civil().year == year && cursor.civil().month == month {
                    let civil = cursor.civil();
                    let week = untagged(civil).week;
                    assert!(week <= 4, "{civil} computed week {week}");
                    cursor = cursor.plus_days(1);
                }
            }
        }
    }

    #[test]
    fn a_twenty_eight_day_february_stops_at_week_three() {
        // 2027 is not a leap year, and 2027-02-01 is a Monday, so its
        // Tuesdays are the 2nd, 9th, 16th, and 23rd.
        let weeks: Vec<u32> = cadence_days(2027, 2)
            .into_iter()
            .map(|date| untagged(date).week)
            .collect();
        assert_eq!(weeks, vec![0, 1, 2, 3]);
        assert_eq!(untagged(date(2027, 2, 28)).week, 3);
    }

    #[test]
    fn the_short_last_week_still_numbers_its_days() {
        // Week 4 holds whatever the month has past day 28: one day in a leap
        // February, two in a 30-day month, three in a 31-day one.
        assert_eq!(untagged(date(2028, 2, 29)).week, 4);
        assert_eq!(untagged(date(2026, 9, 30)).week, 4);
        assert_eq!(untagged(date(2026, 8, 31)).week, 4);
    }
}
