//! Day words shared by the CLI, MCP server and TUI, so `fri` means the same
//! thing everywhere.

use chrono::{Datelike, Days, NaiveDate, Weekday};

use crate::model::Todo;

/// The Monday of the week containing `day`.
pub fn week_start(day: NaiveDate) -> NaiveDate {
    day - Days::new(u64::from(day.weekday().num_days_from_monday()))
}

/// Parses a day relative to `today`: `today`, `tomorrow`, a weekday name
/// (`mon`, `friday`…, the next one on or after today) or `YYYY-MM-DD`.
pub fn parse_day(s: &str, today: NaiveDate) -> Option<NaiveDate> {
    let s = s.trim().to_lowercase();
    match s.as_str() {
        "today" => return Some(today),
        "tomorrow" => return today.succ_opt(),
        _ => {}
    }
    if let Some(weekday) = weekday(&s) {
        let ahead =
            (7 + weekday.num_days_from_monday() - today.weekday().num_days_from_monday()) % 7;
        return today.checked_add_days(Days::new(u64::from(ahead)));
    }
    NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()
}

fn weekday(s: &str) -> Option<Weekday> {
    let names = [
        ("mon", Weekday::Mon),
        ("tue", Weekday::Tue),
        ("wed", Weekday::Wed),
        ("thu", Weekday::Thu),
        ("fri", Weekday::Fri),
        ("sat", Weekday::Sat),
        ("sun", Weekday::Sun),
    ];
    names
        .iter()
        .find(|(short, day)| s == *short || s == day_name(*day))
        .map(|(_, day)| *day)
}

fn day_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "monday",
        Weekday::Tue => "tuesday",
        Weekday::Wed => "wednesday",
        Weekday::Thu => "thursday",
        Weekday::Fri => "friday",
        Weekday::Sat => "saturday",
        Weekday::Sun => "sunday",
    }
}

/// Which todos to keep by scheduled date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledFilter {
    /// No scheduled date.
    Unscheduled,
    /// Scheduled within this inclusive range.
    Range(NaiveDate, NaiveDate),
}

impl ScheduledFilter {
    pub fn matches(&self, todo: &Todo) -> bool {
        match (self, todo.scheduled) {
            (ScheduledFilter::Unscheduled, s) => s.is_none(),
            (ScheduledFilter::Range(from, to), Some(d)) => *from <= d && d <= *to,
            (ScheduledFilter::Range(..), None) => false,
        }
    }
}

/// Parses `none`, `week` (this Monday–Sunday) or any [`parse_day`] word.
pub fn parse_scheduled_filter(s: &str, today: NaiveDate) -> Option<ScheduledFilter> {
    match s.trim().to_lowercase().as_str() {
        "none" => Some(ScheduledFilter::Unscheduled),
        "week" => {
            let monday = week_start(today);
            Some(ScheduledFilter::Range(monday, monday + Days::new(6)))
        }
        other => parse_day(other, today).map(|d| ScheduledFilter::Range(d, d)),
    }
}
