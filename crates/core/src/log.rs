use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::Todo;

/// Which frontend made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Cli,
    Tui,
    Mcp,
    #[default]
    Unknown,
}

/// Who made a change: the frontend plus, when known, the client behind it
/// (an MCP client's name, or whatever `$AGENTODO_ACTOR` says).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Actor {
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

impl Actor {
    /// An actor for `source`, labelled by `$AGENTODO_ACTOR` if it is set.
    pub fn from_env(source: Source) -> Self {
        let client = std::env::var("AGENTODO_ACTOR")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Actor { source, client }
    }
}

/// One change to one todo, with full snapshots so it can be reverted exactly.
///
/// `before` is `None` for a todo coming into existence, `after` is `None`
/// for one being removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: DateTime<Utc>,
    pub actor: Actor,
    /// The store operation that produced it (`add`, `update`, `delete`, …).
    pub op: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Todo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Todo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Created,
    Updated,
    Deleted,
}

/// One field that differs between an entry's snapshots, rendered as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    pub field: &'static str,
    pub old: String,
    pub new: String,
}

impl LogEntry {
    pub fn kind(&self) -> EntryKind {
        match (&self.before, &self.after) {
            (None, _) => EntryKind::Created,
            (Some(_), None) => EntryKind::Deleted,
            (Some(_), Some(_)) => EntryKind::Updated,
        }
    }

    /// The todo's title as of this entry.
    pub fn title(&self) -> &str {
        self.after
            .as_ref()
            .or(self.before.as_ref())
            .map_or("", |t| t.title.as_str())
    }

    /// User-visible fields that changed; empty unless both snapshots exist.
    pub fn diff(&self) -> Vec<FieldChange> {
        let (Some(a), Some(b)) = (&self.before, &self.after) else {
            return Vec::new();
        };
        let lower = |s: String| s.to_lowercase();
        let opt = |o: &Option<String>| o.clone().unwrap_or_default();
        let day = |d: &Option<chrono::NaiveDate>| d.map(|d| d.to_string()).unwrap_or_default();
        let spent = |t: &Todo| match t.sessions.len() {
            0 => String::new(),
            _ => crate::timer::format_duration(t.time_spent()),
        };
        let fields = [
            ("title", a.title.clone(), b.title.clone()),
            (
                "status",
                lower(format!("{:?}", a.status)),
                lower(format!("{:?}", b.status)),
            ),
            (
                "priority",
                lower(format!("{:?}", a.priority)),
                lower(format!("{:?}", b.priority)),
            ),
            ("project", opt(&a.project), opt(&b.project)),
            ("tags", a.tags.join(" "), b.tags.join(" ")),
            ("notes", opt(&a.notes), opt(&b.notes)),
            ("scheduled", day(&a.scheduled), day(&b.scheduled)),
            ("time", spent(a), spent(b)),
        ];
        fields
            .into_iter()
            .filter(|(_, old, new)| old != new)
            .map(|(field, old, new)| FieldChange { field, old, new })
            .collect()
    }
}

/// Where the log for the store at `store_path` lives:
/// `todos.json` → `todos.log.jsonl`.
pub fn log_path(store_path: &Path) -> std::path::PathBuf {
    store_path.with_extension("log.jsonl")
}

/// Reads every entry in the log. A missing log is empty; malformed lines are
/// skipped.
pub fn read_log(path: &Path) -> Result<Vec<LogEntry>> {
    read_log_from(path, 0).map(|(entries, _)| entries)
}

/// Reads complete entries appended after byte `offset`, returning them and
/// the offset to continue from. A log shorter than `offset` (replaced or
/// truncated) is read from the start.
pub fn read_log_from(path: &Path, offset: u64) -> Result<(Vec<LogEntry>, u64)> {
    let io_err = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(io_err(e)),
    };
    let len = file.metadata().map_err(io_err)?.len();
    let offset = if len < offset { 0 } else { offset };
    file.seek(SeekFrom::Start(offset)).map_err(io_err)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(io_err)?;

    // Only consume whole lines, so a line being written right now is picked
    // up on the next read instead of being lost.
    let complete = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let entries = String::from_utf8_lossy(&buf[..complete])
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok((entries, offset + complete as u64))
}
