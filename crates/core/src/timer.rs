use chrono::{DateTime, Duration, Utc};

use crate::model::ActiveSession;

/// Length of a focus session when none is given: `$AGENTODO_TIMER_MINUTES`,
/// else 25 minutes.
pub fn default_timer() -> Duration {
    std::env::var("AGENTODO_TIMER_MINUTES")
        .ok()
        .and_then(|m| m.trim().parse::<i64>().ok())
        .filter(|&m| m > 0)
        .map_or(Duration::minutes(25), Duration::minutes)
}

/// Parses a session length: a bare number of minutes (`25`) or units
/// (`50m`, `1h`, `1h30m`, `90s`). Zero and negative lengths are rejected.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim().to_ascii_lowercase();
    if let Ok(minutes) = s.parse::<i64>() {
        return (minutes > 0).then(|| Duration::minutes(minutes));
    }
    let mut total = Duration::zero();
    let mut digits = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let n: i64 = std::mem::take(&mut digits).parse().ok()?;
        total += match c {
            'h' => Duration::hours(n),
            'm' => Duration::minutes(n),
            's' => Duration::seconds(n),
            _ => return None,
        };
    }
    (digits.is_empty() && total > Duration::zero()).then_some(total)
}

/// A duration for humans: `1h12m`, `25m`, `40s`. Seconds only show under a
/// minute.
pub fn format_duration(d: Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match (h, m) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h{m}m"),
    }
}

/// A countdown clock: `18:42`, or `1:02:03` past an hour.
pub fn format_countdown(d: Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// A running timer's clock: time left in the block (`18:42`), then the
/// overtime past it (`+01:12`, `+9:25:33`).
pub fn format_timer(active: &ActiveSession, now: DateTime<Utc>) -> String {
    if active.is_over(now) {
        format!("+{}", format_countdown(active.overtime(now)))
    } else {
        format_countdown(active.remaining(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_takes_minutes_and_units() {
        assert_eq!(parse_duration("25"), Some(Duration::minutes(25)));
        assert_eq!(parse_duration(" 50m "), Some(Duration::minutes(50)));
        assert_eq!(parse_duration("1h30m"), Some(Duration::minutes(90)));
        assert_eq!(parse_duration("1H"), Some(Duration::hours(1)));
        assert_eq!(parse_duration("90s"), Some(Duration::seconds(90)));
        for bad in ["", "0", "-5", "m", "10x", "1h30", "abc"] {
            assert_eq!(parse_duration(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn durations_format_compactly() {
        assert_eq!(format_duration(Duration::seconds(40)), "40s");
        assert_eq!(format_duration(Duration::minutes(25)), "25m");
        assert_eq!(format_duration(Duration::minutes(60)), "1h");
        assert_eq!(format_duration(Duration::minutes(72)), "1h12m");
        assert_eq!(format_countdown(Duration::seconds(1122)), "18:42");
        assert_eq!(format_countdown(Duration::seconds(3723)), "1:02:03");
        assert_eq!(format_countdown(Duration::seconds(-5)), "00:00");
    }

    #[test]
    fn the_timer_counts_down_then_up() {
        let start: DateTime<Utc> = "2026-10-05T09:00:00Z".parse().unwrap();
        let active = ActiveSession {
            todo: 1,
            started_at: start,
            planned_secs: 25 * 60,
        };
        let at = |secs| start + Duration::seconds(secs);
        assert_eq!(format_timer(&active, at(378)), "18:42");
        assert_eq!(format_timer(&active, at(1500)), "+00:00");
        assert_eq!(format_timer(&active, at(1500 + 72)), "+01:12");
        assert_eq!(format_timer(&active, at(1500 + 33933)), "+9:25:33");
    }
}
