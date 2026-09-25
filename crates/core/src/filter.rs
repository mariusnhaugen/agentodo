use crate::date::ScheduledFilter;
use crate::model::{Priority, Status, Todo};

/// How to order a list of todos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// Highest priority first, newest first as a tie-breaker.
    #[default]
    Priority,
    /// Most recently created first.
    Created,
}

/// Criteria for narrowing a list of todos. An unset field does not constrain.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub status: Option<Status>,
    pub priority: Option<Priority>,
    pub tag: Option<String>,
    pub project: Option<String>,
    pub scheduled: Option<ScheduledFilter>,
    pub sort: SortBy,
}

impl Filter {
    /// Returns true if `todo` satisfies every set criterion.
    pub fn matches(&self, todo: &Todo) -> bool {
        if let Some(status) = self.status {
            if todo.status != status {
                return false;
            }
        }
        if let Some(priority) = self.priority {
            if todo.priority != priority {
                return false;
            }
        }
        if let Some(tag) = &self.tag {
            if !todo.tags.iter().any(|t| t == tag) {
                return false;
            }
        }
        if let Some(project) = &self.project {
            if todo.project.as_deref() != Some(project.as_str()) {
                return false;
            }
        }
        if let Some(scheduled) = &self.scheduled {
            if !scheduled.matches(todo) {
                return false;
            }
        }
        true
    }

    /// Sorts `todos` in place according to `self.sort`.
    pub fn sort(&self, todos: &mut [Todo]) {
        match self.sort {
            SortBy::Priority => {
                // Priority descending, then newest first.
                todos.sort_by(|a, b| {
                    b.priority
                        .cmp(&a.priority)
                        .then_with(|| b.created_at.cmp(&a.created_at))
                });
            }
            SortBy::Created => {
                todos.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            }
        }
    }
}
