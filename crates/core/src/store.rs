use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::filter::Filter;
use crate::model::{NewTodo, ProjectSummary, Status, Todo, TodoPatch};

/// The on-disk representation of the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreData {
    next_id: u64,
    todos: Vec<Todo>,
}

impl Default for StoreData {
    fn default() -> Self {
        StoreData {
            next_id: 1,
            todos: Vec::new(),
        }
    }
}

/// A collection of todos backed by a single JSON file.
///
/// Mutations happen in memory; call [`Store::save`] to persist them. Saving is
/// atomic — data is written to a temp file and renamed over the target, so an
/// interrupted write cannot corrupt an existing store.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    data: StoreData,
}

impl Store {
    /// Resolves the default store path: `<data_dir>/agentodo/todos.json`.
    pub fn default_path() -> Result<PathBuf> {
        let dirs =
            directories::ProjectDirs::from("", "", "agentodo").ok_or(Error::NoDataDir)?;
        Ok(dirs.data_dir().join("todos.json"))
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
        Ok(Store { path, data })
    }

    /// The path this store reads from and writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Persists the store to disk atomically.
    pub fn save(&self) -> Result<()> {
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
        })
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
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        self.data.todos.push(todo.clone());
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
        for tag in patch.remove_tags {
            todo.tags.retain(|t| t != &tag);
        }
        for tag in patch.add_tags {
            if !todo.tags.contains(&tag) {
                todo.tags.push(tag);
            }
        }
        todo.updated_at = Utc::now();
        Ok(todo.clone())
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

        todo.status = status;
        todo.completed_at = match status {
            Status::Done => Some(now),
            Status::Open => None,
        };
        todo.updated_at = now;
        Ok(todo.clone())
    }

    /// Removes a todo by id, returning it.
    pub fn delete(&mut self, id: u64) -> Result<Todo> {
        let pos = self
            .data
            .todos
            .iter()
            .position(|t| t.id == id)
            .ok_or(Error::NotFound(id))?;
        Ok(self.data.todos.remove(pos))
    }
}
