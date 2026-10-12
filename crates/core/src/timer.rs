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

/// How long a timer may run in overtime before pausing itself:
/// `$AGENTODO_TIMER_MAX_OVER` (`2h`, `90`, or `off` for never), else an hour.
pub fn max_overtime() -> Option<Duration> {
    match std::env::var("AGENTODO_TIMER_MAX_OVER") {
        Ok(v) if matches!(v.trim(), "off" | "none" | "0") => None,
        Ok(v) => Some(parse_duration(&v).unwrap_or(Duration::hours(1))),
        Err(_) => Some(Duration::hours(1)),
    }
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
/// overtime past it (`+01:12`, `+9:25:33`). Paused, it leads with `⏸`.
pub fn format_timer(active: &ActiveSession, now: DateTime<Utc>) -> String {
    let clock = if active.is_over(now) {
        format!("+{}", format_countdown(active.overtime(now)))
    } else {
        format_countdown(active.remaining(now))
    };
    if active.is_paused(now) {
        format!("⏸ {clock}")
    } else {
        clock
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
            paused_secs: 0,
            paused_at: None,
            max_over_secs: None,
            resumed_at: None,
        };
        let at = |secs| start + Duration::seconds(secs);
        assert_eq!(format_timer(&active, at(378)), "18:42");
        assert_eq!(format_timer(&active, at(1500)), "+00:00");
        assert_eq!(format_timer(&active, at(1500 + 72)), "+01:12");
        assert_eq!(format_timer(&active, at(1500 + 33933)), "+9:25:33");
    }

    #[test]
    fn a_paused_timer_stands_still() {
        let start: DateTime<Utc> = "2026-10-05T09:00:00Z".parse().unwrap();
        let at = |secs| start + Duration::seconds(secs);
        let mut active = ActiveSession {
            todo: 1,
            started_at: start,
            planned_secs: 25 * 60,
            paused_secs: 0,
            paused_at: None,
            max_over_secs: None,
            resumed_at: None,
        };
        active.pause(at(378));
        assert_eq!(format_timer(&active, at(378)), "⏸ 18:42");
        assert_eq!(format_timer(&active, at(5000)), "⏸ 18:42");
        assert_eq!(active.ends_at(at(5000)), None);

        active.resume(at(978));
        assert_eq!(format_timer(&active, at(978)), "18:42");
        assert_eq!(active.ends_at(at(978)), Some(at(2100)));
        assert_eq!(format_timer(&active, at(2100 + 72)), "+01:12");
    }

    #[test]
    fn a_timer_left_running_pauses_itself() {
        let start: DateTime<Utc> = "2026-10-05T09:00:00Z".parse().unwrap();
        let at = |mins| start + Duration::minutes(mins);
        let mut active = ActiveSession {
            todo: 1,
            started_at: start,
            planned_secs: 25 * 60,
            paused_secs: 0,
            paused_at: None,
            max_over_secs: Some(60 * 60),
            resumed_at: None,
        };
        assert_eq!(active.auto_pause_at(), Some(at(85)));
        assert!(!active.is_paused(at(84)));
        assert_eq!(format_timer(&active, at(84)), "+59:00");

        // Friday to Monday: the clock stopped an hour into overtime.
        let monday = at(3 * 24 * 60);
        assert!(active.is_auto_paused(monday));
        assert_eq!(format_timer(&active, monday), "⏸ +1:00:00");
        assert_eq!(active.close_at(monday).end, at(85));
        assert_eq!(active.ends_at(monday), None);

        // Resuming gives it another hour from then.
        active.resume(monday);
        assert!(!active.is_paused(monday));
        assert_eq!(active.overtime(monday), Duration::hours(1));
        assert_eq!(active.auto_pause_at(), Some(monday + Duration::hours(1)));

        // Paused by hand, it stays where it was paused.
        active.pause(monday + Duration::minutes(5));
        assert!(!active.is_auto_paused(monday + Duration::hours(9)));
        assert_eq!(
            active.overtime(monday + Duration::hours(9)),
            Duration::minutes(65)
        );

        // Pausing an auto-paused timer pins it where it stopped.
        let mut idle = ActiveSession {
            paused_at: None,
            paused_secs: 0,
            resumed_at: None,
            ..active
        };
        idle.pause(monday);
        assert_eq!(idle.paused_at, Some(at(85)));
    }
}
