use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::log::{self, Actor, LogEntry};
use crate::model::{ActiveSession, NewTodo, ProjectSummary, Session, Status, Todo, TodoPatch};

/// The on-disk representation of the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreData {
    next_id: u64,
    todos: Vec<Todo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_session: Option<ActiveSession>,
}

impl Default for StoreData {
    fn default() -> Self {
        StoreData {
            next_id: 1,
            todos: Vec::new(),
            active_session: None,
        }
    }
}

/// A collection of todos backed by a single JSON file.
///
/// Mutations happen in memory; call [`Store::save`] to persist them. Saving is
/// atomic — data is written to a temp file and renamed over the target, so an
/// interrupted write cannot corrupt an existing store.
///
/// Every mutation is also recorded as a [`LogEntry`] attributed to the store's
/// [`Actor`], and appended to the change log (see [`log::log_path`]) by `save`.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    data: StoreData,
    actor: Actor,
    /// Log entries for mutations not yet saved.
    pending: Vec<LogEntry>,
}

impl Store {
    /// Resolves the default store path: `<data_dir>/agentodo/todos.json`.
    pub fn default_path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "agentodo").ok_or(Error::NoDataDir)?;
        Ok(dirs.data_dir().join("todos.json"))
    }

    /// Resolves the store path with the precedence every frontend shares:
    /// an explicit `flag`, then `$AGENTODO_STORE`, then [`Store::default_path`].
    pub fn resolve_path(flag: Option<PathBuf>) -> Result<PathBuf> {
        if let Some(p) = flag {
            return Ok(p);
        }
        if let Some(p) = std::env::var_os("AGENTODO_STORE") {
            return Ok(PathBuf::from(p));
        }
        Self::default_path()
    }

    /// Loads the store at `path`, or returns an empty store if the file does
    /// not exist yet.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let data = match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).map_err(|source| Error::Parse {
                path: path.clone(),
                source,
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => StoreData::default(),
            Err(source) => {
                return Err(Error::Io {
                    path: path.clone(),
                    source,
                })
            }
        };
        Ok(Store {
            path,
            data,
            actor: Actor::default(),
            pending: Vec::new(),
        })
    }

    /// The path this store reads from and writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Attributes subsequent mutations to `actor` in the change log.
    pub fn set_actor(&mut self, actor: Actor) {
        self.actor = actor;
    }

    /// The change log belonging to this store.
    pub fn log_path(&self) -> PathBuf {
        log::log_path(&self.path)
    }

    fn record(&mut self, op: &str, id: u64, before: Option<Todo>, after: Option<Todo>) {
        // `updated_at` alone moving isn't a change worth logging.
        if let (Some(b), Some(a)) = (&before, &after) {
            let unchanged = Todo {
                updated_at: a.updated_at,
                ..b.clone()
            } == *a;
            if unchanged {
                return;
            }
        }
        self.pending.push(LogEntry {
            at: Utc::now(),
            actor: self.actor.clone(),
            op: op.to_owned(),
            id,
            before,
            after,
        });
    }

    /// Persists the store to disk atomically, then appends the change log.
    ///
    /// The log is written after the store, so an interrupted save can lose a
    /// log line but never log a change that didn't happen.
    pub fn save(&mut self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }

        let json = serde_json::to_string_pretty(&self.data).map_err(Error::Serialize)?;

        // Write to a sibling temp file, then rename over the target so readers
        // never observe a partially written file.
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&tmp).map_err(|source| Error::Io {
                path: tmp.clone(),
                source,
            })?;
            file.write_all(json.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|source| Error::Io {
                    path: tmp.clone(),
                    source,
                })?;
        }
        fs::rename(&tmp, &self.path).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })?;

        self.append_log()
    }

    fn append_log(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut lines = String::new();
        for entry in self.pending.drain(..) {
            lines.push_str(&serde_json::to_string(&entry).map_err(Error::Serialize)?);
            lines.push('\n');
        }
        let path = self.log_path();
        // One append-mode write per save keeps concurrent writers' lines whole.
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(lines.as_bytes()))
            .map_err(|source| Error::Io { path, source })
    }

    /// All todos in insertion order (unsorted, unfiltered).
    pub fn all(&self) -> &[Todo] {
        &self.data.todos
    }

    /// Returns todos matching `filter`, sorted per the filter's sort key.
    pub fn list(&self, filter: &Filter) -> Vec<Todo> {
        let mut result: Vec<Todo> = self
            .data
            .todos
            .iter()
            .filter(|t| filter.matches(t))
            .cloned()
            .collect();
        filter.sort(&mut result);
        result
    }

    /// Distinct projects with open/done counts, alphabetical, unassigned last.
    ///
    /// Todos with no project (or an empty one, which only a hand-edited file
    /// can produce) are folded into a single trailing `None` bucket. Matching
    /// is case-sensitive, so `Work` and `work` are reported separately — that
    /// is what makes an accidental duplicate visible.
    pub fn projects(&self) -> Vec<ProjectSummary> {
        if self.data.todos.is_empty() {
            return Vec::new();
        }

        let mut counts: BTreeMap<Option<&str>, (usize, usize)> = BTreeMap::new();
        for todo in &self.data.todos {
            let key = todo.project.as_deref().filter(|p| !p.is_empty());
            let entry = counts.entry(key).or_default();
            match todo.status {
                Status::Open => entry.0 += 1,
                Status::Done => entry.1 += 1,
            }
        }

        // `None` sorts first under `Option`'s ordering, but it belongs last.
        let unassigned = counts.remove(&None);
        let mut summaries: Vec<ProjectSummary> = counts
            .into_iter()
            .map(|(project, (open, done))| ProjectSummary {
                project: project.map(str::to_owned),
                open,
                done,
            })
            .collect();
        let (open, done) = unassigned.unwrap_or_default();
        summaries.push(ProjectSummary {
            project: None,
            open,
            done,
        });
        summaries
    }

    /// Looks up a todo by id.
    pub fn get(&self, id: u64) -> Option<&Todo> {
        self.data.todos.iter().find(|t| t.id == id)
    }

    /// Creates a new todo, assigns it the next id, and returns it.
    pub fn add(&mut self, new: NewTodo) -> Todo {
        let now = Utc::now();
        let id = self.data.next_id;
        self.data.next_id += 1;

        let todo = Todo {
            id,
            title: new.title,
            status: Status::Open,
            priority: new.priority,
            project: new.project,
            tags: new.tags,
            notes: new.notes,
            scheduled: new.scheduled,
            plan_secs: new.plan.map(plan_secs),
            created_at: now,
            updated_at: now,
            completed_at: None,
            sessions: Vec::new(),
        };
        self.data.todos.push(todo.clone());
        self.record("add", id, None, Some(todo.clone()));
        todo
    }

    /// Applies a partial update to the todo with the given id.
    pub fn update(&mut self, id: u64, patch: TodoPatch) -> Result<Todo> {
        let todo = self
            .data
            .todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(Error::NotFound(id))?;
        let before = todo.clone();

        if let Some(title) = patch.title {
            todo.title = title;
        }
        if let Some(priority) = patch.priority {
            todo.priority = priority;
        }
        if let Some(project) = patch.project {
            todo.project = project;
        }
        if let Some(notes) = patch.notes {
            todo.notes = notes;
        }
        if let Some(scheduled) = patch.scheduled {
            todo.scheduled = scheduled;
        }
        if let Some(plan) = patch.plan {
            todo.plan_secs = plan.map(plan_secs);
        }
        for tag in patch.remove_tags {
            todo.tags.retain(|t| t != &tag);
        }
        for tag in patch.add_tags {
            if !todo.tags.contains(&tag) {
                todo.tags.push(tag);
            }
        }
        todo.updated_at = Utc::now();
        let after = todo.clone();
        self.record("update", id, Some(before), Some(after.clone()));
        Ok(after)
    }

    /// Sets a todo's status, maintaining `completed_at`.
    pub fn set_status(&mut self, id: u64, status: Status) -> Result<Todo> {
        let now = Utc::now();
        let todo = self
            .data
            .todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(Error::NotFound(id))?;
        let before = todo.clone();

        // Finishing a todo ends its focus session; the time so far counts.
        if status == Status::Done {
            if let Some(active) = self.data.active_session.filter(|a| a.todo == id) {
                todo.sessions.push(active.close_at(now));
                self.data.active_session = None;
            }
        }
        todo.status = status;
        todo.completed_at = match status {
            Status::Done => Some(now),
            Status::Open => None,
        };
        todo.updated_at = now;
        let after = todo.clone();
        self.record("set_status", id, Some(before), Some(after.clone()));
        Ok(after)
    }

    /// Removes a todo by id, returning it.
    pub fn delete(&mut self, id: u64) -> Result<Todo> {
        let pos = self
            .data
            .todos
            .iter()
            .position(|t| t.id == id)
            .ok_or(Error::NotFound(id))?;
        let todo = self.data.todos.remove(pos);
        if self.data.active_session.is_some_and(|a| a.todo == id) {
            self.data.active_session = None;
        }
        self.record("delete", id, Some(todo.clone()), None);
        Ok(todo)
    }

    /// Re-inserts a previously removed todo verbatim, keeping its id and
    /// timestamps. Used to undo a delete without the todo changing identity.
    pub fn restore(&mut self, todo: Todo) -> Result<Todo> {
        if self.get(todo.id).is_some() {
            return Err(Error::IdTaken(todo.id));
        }
        self.data.next_id = self.data.next_id.max(todo.id + 1);
        self.data.todos.push(todo.clone());
        self.record("restore", todo.id, None, Some(todo.clone()));
        Ok(todo)
    }

    /// Overwrites the todo with the same id with `todo` verbatim, timestamps
    /// included. Used to reapply an undo/redo snapshot.
    ///
    /// Sessions are the exception: the todo keeps the ones it has now, so
    /// restoring an older snapshot never throws away tracked time.
    pub fn replace(&mut self, mut todo: Todo) -> Result<Todo> {
        let slot = self
            .data
            .todos
            .iter_mut()
            .find(|t| t.id == todo.id)
            .ok_or(Error::NotFound(todo.id))?;
        todo.sessions = slot.sessions.clone();
        let before = std::mem::replace(slot, todo.clone());
        self.record("replace", todo.id, Some(before), Some(todo.clone()));
        Ok(todo)
    }

    // ---- focus timer ----------------------------------------------------------

    /// The running timer, if any. It may be past its planned block, in
    /// overtime; it only stops when something stops it.
    pub fn active_timer(&self) -> Option<&ActiveSession> {
        self.data.active_session.as_ref()
    }

    /// Starts a focus timer with a planned block of `length` on todo `id`.
    /// A timer already running (on any todo) is stopped first and recorded.
    pub fn start_timer(
        &mut self,
        id: u64,
        length: Duration,
        now: DateTime<Utc>,
    ) -> Result<ActiveSession> {
        if self.get(id).is_none() {
            return Err(Error::NotFound(id));
        }
        self.close_active(now);
        let active = ActiveSession {
            todo: id,
            started_at: now,
            planned_secs: length.num_seconds().max(1) as u64,
            paused_secs: 0,
            paused_at: None,
        };
        self.data.active_session = Some(active);
        Ok(active)
    }

    /// Pauses the running timer at `now`: its clock stands still and the
    /// pause isn't counted as work. Returns the timer, `None` if none runs.
    pub fn pause_timer(&mut self, now: DateTime<Utc>) -> Option<ActiveSession> {
        let active = self.data.active_session.as_mut()?;
        active.pause(now);
        Some(*active)
    }

    /// Resumes a paused timer at `now`. Returns the timer, `None` if none
    /// runs.
    pub fn resume_timer(&mut self, now: DateTime<Utc>) -> Option<ActiveSession> {
        let active = self.data.active_session.as_mut()?;
        active.resume(now);
        Some(*active)
    }

    /// Stops the running timer, recording the session up to `now`, overtime
    /// included. Returns the todo it was recorded on.
    pub fn stop_timer(&mut self, now: DateTime<Utc>) -> Option<Todo> {
        self.close_active(now)
    }

    /// Changes how long session `index` (oldest first) lasted, keeping its
    /// start and pauses. For fixing a timer that was left running.
    pub fn set_session_length(&mut self, id: u64, index: usize, length: Duration) -> Result<Todo> {
        self.edit_sessions(id, |sessions| {
            let session = sessions.get_mut(index)?;
            session.end = session.start + session.paused() + length.max(Duration::zero());
            Some(())
        })
        .and_then(|t| t.ok_or(Error::NoSession { id, index }))
    }

    /// Removes session `index` (oldest first).
    pub fn delete_session(&mut self, id: u64, index: usize) -> Result<Todo> {
        self.edit_sessions(id, |sessions| {
            (index < sessions.len()).then(|| {
                sessions.remove(index);
            })
        })
        .and_then(|t| t.ok_or(Error::NoSession { id, index }))
    }

    /// Records `length` of work done without the timer, ending at `now`.
    pub fn add_session(&mut self, id: u64, length: Duration, now: DateTime<Utc>) -> Result<Todo> {
        self.edit_sessions(id, |sessions| {
            sessions.push(Session {
                start: now - length,
                end: now,
                planned_secs: None,
                paused_secs: 0,
            });
            Some(())
        })
        .map(|t| t.expect("pushing always succeeds"))
    }

    /// Overwrites a todo's sessions wholesale. Used to undo session edits,
    /// which [`Store::replace`] deliberately leaves alone.
    pub fn set_sessions(&mut self, id: u64, sessions: Vec<Session>) -> Result<Todo> {
        self.edit_sessions(id, |s| {
            *s = sessions;
            Some(())
        })
        .map(|t| t.expect("setting always succeeds"))
    }

    /// Applies `f` to a todo's sessions and logs the change. `Ok(None)` when
    /// `f` declines (an index out of range), with nothing changed.
    fn edit_sessions(
        &mut self,
        id: u64,
        f: impl FnOnce(&mut Vec<Session>) -> Option<()>,
    ) -> Result<Option<Todo>> {
        let todo = self
            .data
            .todos
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(Error::NotFound(id))?;
        let before = todo.clone();
        if f(&mut todo.sessions).is_none() {
            return Ok(None);
        }
        todo.updated_at = Utc::now();
        let after = todo.clone();
        self.record("session", id, Some(before), Some(after.clone()));
        Ok(Some(after))
    }

    /// Clears the running timer and appends its session to its todo.
    /// Zero-length sessions are dropped rather than recorded.
    fn close_active(&mut self, now: DateTime<Utc>) -> Option<Todo> {
        let active = self.data.active_session.take()?;
        let session = active.close_at(now);
        if session.duration() <= Duration::zero() {
            return None;
        }
        let todo = self.data.todos.iter_mut().find(|t| t.id == active.todo)?;
        let before = todo.clone();
        todo.sessions.push(session);
        todo.updated_at = now;
        let after = todo.clone();
        self.record("session", active.todo, Some(before), Some(after.clone()));
        Some(after)
    }
}

/// A plan as stored: whole seconds, never negative.
fn plan_secs(plan: Duration) -> u64 {
    plan.num_seconds().max(0) as u64
}
