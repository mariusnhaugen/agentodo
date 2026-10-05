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
}

impl Session {
    pub fn duration(&self) -> Duration {
        self.end - self.start
    }

    /// How far it ran past its planned block, if it did.
    pub fn overtime(&self) -> Option<Duration> {
        let planned = Duration::seconds(self.planned_secs? as i64);
        Some(self.duration() - planned).filter(|over| *over > Duration::zero())
    }
}

/// The running focus timer. There is at most one, stored beside the todos so
/// every frontend sees it; the countdown is derived from the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSession {
    pub todo: u64,
    pub started_at: DateTime<Utc>,
    pub planned_secs: u64,
}

impl ActiveSession {
    /// When the planned block is up. The timer keeps running past it,
    /// counting overtime, until it is stopped.
    pub fn ends_at(&self) -> DateTime<Utc> {
        self.started_at + Duration::seconds(self.planned_secs as i64)
    }

    /// Time left in the block at `now`, never negative.
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        (self.ends_at() - now).max(Duration::zero())
    }

    /// Time past the block at `now`, never negative.
    pub fn overtime(&self, now: DateTime<Utc>) -> Duration {
        (now - self.ends_at()).max(Duration::zero())
    }

    pub fn is_over(&self, now: DateTime<Utc>) -> bool {
        now >= self.ends_at()
    }

    /// The session as recorded if it stops at `now`, overtime included.
    pub fn close_at(&self, now: DateTime<Utc>) -> Session {
        Session {
            start: self.started_at,
            end: now.max(self.started_at),
            planned_secs: Some(self.planned_secs),
        }
    }
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
