use chrono::{DateTime, NaiveDate, Utc};
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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
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
}

/// A partial update to an existing todo. `None` fields are left unchanged.
///
/// `project`, `notes` and `scheduled` are doubly-wrapped: the outer `Option` means "was this
/// field mentioned at all?", the inner value (which may itself clear the field)
/// is what to set it to.
#[derive(Debug, Clone, Default)]
pub struct TodoPatch {
    pub title: Option<String>,
    pub priority: Option<Priority>,
    pub project: Option<Option<String>>,
    pub notes: Option<Option<String>>,
    pub scheduled: Option<Option<NaiveDate>>,
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
