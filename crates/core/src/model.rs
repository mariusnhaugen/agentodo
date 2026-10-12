use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// How urgent a todo is. Ordered so `High > Medium > Low` for sorting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Medium,
    High,
}

/// Whether a todo is still outstanding or finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Open,
    Done,
}

/// A single todo item. `id` is assigned by the store and is stable for the
/// lifetime of the item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub id: u64,
    pub title: String,
    pub status: Status,
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The day you plan to work on it. A soft "do-on" date, never a deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<NaiveDate>,
    /// Time budgeted for the whole todo, across all its sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_secs: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Finished focus sessions, oldest first. Snapshot restores (undo,
    /// revert) keep whatever sessions the todo has by then; sessions change
    /// only through the store's session methods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<Session>,
}

impl Todo {
    /// Total time across finished sessions.
    pub fn time_spent(&self) -> Duration {
        self.sessions.iter().map(Session::duration).sum()
    }

    /// The time budgeted for this todo, if any.
    pub fn plan(&self) -> Option<Duration> {
        self.plan_secs.map(|s| Duration::seconds(s as i64))
    }

    /// What's left of the plan after the recorded time plus `running` (a
    /// timer still going on it). Zero once the budget is used up; `None`
    /// without a plan.
    pub fn plan_left(&self, running: Duration) -> Option<Duration> {
        let left = self.plan()? - self.time_spent() - running;
        Some(left.max(Duration::zero()))
    }
}

/// One finished stretch of work on a todo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// The block the timer was started for; `None` for time added by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_secs: Option<u64>,
    /// Time the timer spent paused between `start` and `end`, not worked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub paused_secs: u64,
}

impl Session {
    /// Time worked: `start` to `end`, less any pauses.
    pub fn duration(&self) -> Duration {
        self.end - self.start - self.paused()
    }

    pub fn paused(&self) -> Duration {
        Duration::seconds(self.paused_secs as i64)
    }

    /// How far it ran past its planned block, if it did.
    pub fn overtime(&self) -> Option<Duration> {
        let planned = Duration::seconds(self.planned_secs? as i64);
        Some(self.duration() - planned).filter(|over| *over > Duration::zero())
    }
}

/// The running focus timer. There is at most one, stored beside the todos so
/// every frontend sees it; the countdown is derived from the clock.
///
/// A paused timer is still the running timer: its clock stands still from
/// `paused_at` until it is resumed, and the pause isn't counted as work.
///
/// A timer left running pauses itself `max_over_secs` into overtime (or
/// that long after a resume, if later), as though paused right then. It's
/// derived from the clock like the countdown, so it holds whether or not
/// anything was watching when it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSession {
    pub todo: u64,
    pub started_at: DateTime<Utc>,
    pub planned_secs: u64,
    /// Pauses already resumed from.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub paused_secs: u64,
    /// Set while paused by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_at: Option<DateTime<Utc>>,
    /// How long it may run unattended in overtime before pausing itself;
    /// `None` runs on until stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_over_secs: Option<u64>,
    /// The last resume, which restarts the unattended allowance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_at: Option<DateTime<Utc>>,
}

impl ActiveSession {
    /// The planned block.
    pub fn planned(&self) -> Duration {
        Duration::seconds(self.planned_secs as i64)
    }

    /// When a running timer pauses itself, `None` without a limit or once
    /// paused by hand.
    pub fn auto_pause_at(&self) -> Option<DateTime<Utc>> {
        if self.paused_at.is_some() {
            return None;
        }
        let limit = Duration::seconds(self.max_over_secs? as i64);
        let block_end =
            self.started_at + Duration::seconds(self.paused_secs as i64) + self.planned();
        let from = self.resumed_at.map_or(block_end, |r| r.max(block_end));
        Some(from + limit)
    }

    /// When the clock stopped, if it's stopped at `now`: paused by hand, or
    /// paused itself.
    fn paused_since(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.paused_at
            .or(self.auto_pause_at().filter(|at| *at <= now))
    }

    pub fn is_paused(&self, now: DateTime<Utc>) -> bool {
        self.paused_since(now).is_some()
    }

    /// Paused itself, having run past its overtime limit.
    pub fn is_auto_paused(&self, now: DateTime<Utc>) -> bool {
        self.paused_at.is_none() && self.is_paused(now)
    }

    /// Time worked by `now`: the clock from the start, less pauses.
    pub fn elapsed(&self, now: DateTime<Utc>) -> Duration {
        let to = self.paused_since(now).unwrap_or(now);
        (to - self.started_at - Duration::seconds(self.paused_secs as i64)).max(Duration::zero())
    }

    /// When the planned block is up, if it keeps running from `now`; `None`
    /// while paused. The timer keeps running past it, counting overtime,
    /// until it is stopped.
    pub fn ends_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (!self.is_paused(now)).then(|| now + self.planned() - self.elapsed(now))
    }

    /// Time left in the block at `now`, never negative.
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        (self.planned() - self.elapsed(now)).max(Duration::zero())
    }

    /// Time past the block at `now`, never negative.
    pub fn overtime(&self, now: DateTime<Utc>) -> Duration {
        (self.elapsed(now) - self.planned()).max(Duration::zero())
    }

    pub fn is_over(&self, now: DateTime<Utc>) -> bool {
        self.elapsed(now) >= self.planned()
    }

    /// Stops the clock at `now`, or where it already stopped if it paused
    /// itself. No-op if already paused by hand.
    pub fn pause(&mut self, now: DateTime<Utc>) {
        if self.paused_at.is_none() {
            let at = self.paused_since(now).unwrap_or(now);
            self.paused_at = Some(at.max(self.started_at));
        }
    }

    /// Restarts the clock at `now`, with a fresh overtime allowance. No-op
    /// unless paused.
    pub fn resume(&mut self, now: DateTime<Utc>) {
        if let Some(at) = self.paused_since(now) {
            self.paused_secs += (now - at).num_seconds().max(0) as u64;
            self.paused_at = None;
            self.resumed_at = Some(now);
        }
    }

    /// The session as recorded if it stops at `now`, overtime included. A
    /// paused timer's session ends when it was paused.
    pub fn close_at(&self, now: DateTime<Utc>) -> Session {
        Session {
            start: self.started_at,
            end: self.paused_since(now).unwrap_or(now).max(self.started_at),
            planned_secs: Some(self.planned_secs),
            paused_secs: self.paused_secs,
        }
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Fields supplied when creating a new todo. The store fills in `id` and
/// timestamps.
#[derive(Debug, Clone, Default)]
pub struct NewTodo {
    pub title: String,
    pub priority: Priority,
    pub project: Option<String>,
    pub tags: Vec<String>,
    pub notes: Option<String>,
    pub scheduled: Option<NaiveDate>,
    pub plan: Option<Duration>,
}

/// A partial update to an existing todo. `None` fields are left unchanged.
///
/// `project`, `notes`, `scheduled` and `plan` are doubly-wrapped: the outer `Option` means "was this
/// field mentioned at all?", the inner value (which may itself clear the field)
/// is what to set it to.
#[derive(Debug, Clone, Default)]
pub struct TodoPatch {
    pub title: Option<String>,
    pub priority: Option<Priority>,
    pub project: Option<Option<String>>,
    pub notes: Option<Option<String>>,
    pub scheduled: Option<Option<NaiveDate>>,
    pub plan: Option<Option<Duration>>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
}

/// Aggregate todo counts for one project.
///
/// `project` is `None` for todos with no project set; unlike [`Todo::project`]
/// it is always serialized, so consumers see the unassigned bucket as an
/// explicit `null` rather than a missing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project: Option<String>,
    pub open: usize,
    pub done: usize,
}
