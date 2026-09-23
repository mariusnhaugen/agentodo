//! Core domain and storage for agentodo.
//!
//! This crate holds all todo logic behind a small API ([`Store`]) so that the
//! CLI and the MCP server are thin adapters over the same functions.

mod error;
mod filter;
mod log;
mod model;
mod parse;
mod store;

pub use error::{Error, Result};
pub use filter::{Filter, SortBy};
pub use log::{log_path, read_log, read_log_from, Actor, EntryKind, FieldChange, LogEntry, Source};
pub use model::{NewTodo, Priority, ProjectSummary, Status, Todo, TodoPatch};
pub use parse::{parse_quick_add, QuickAdd};
pub use store::Store;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(store: &mut Store) {
        store.add(NewTodo {
            title: "buy milk".into(),
            priority: Priority::Low,
            project: Some("home".into()),
            tags: vec!["errand".into()],
            notes: None,
        });
        store.add(NewTodo {
            title: "ship release".into(),
            priority: Priority::High,
            project: Some("work".into()),
            tags: vec!["urgent".into()],
            notes: Some("cut the tag first".into()),
        });
    }

    #[test]
    fn add_assigns_sequential_ids() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let a = store.add(NewTodo {
            title: "first".into(),
            ..Default::default()
        });
        let b = store.add(NewTodo {
            title: "second".into(),
            ..Default::default()
        });
        assert_eq!(a.id, 1);
        assert_eq!(b.id, 2);
        assert_eq!(b.priority, Priority::Medium); // default
        assert_eq!(b.status, Status::Open);
    }

    #[test]
    fn filter_by_status_priority_tag_project() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        sample(&mut store);

        let high = store.list(&Filter {
            priority: Some(Priority::High),
            ..Default::default()
        });
        assert_eq!(high.len(), 1);
        assert_eq!(high[0].title, "ship release");

        let home = store.list(&Filter {
            project: Some("home".into()),
            ..Default::default()
        });
        assert_eq!(home.len(), 1);
        assert_eq!(home[0].title, "buy milk");

        let errands = store.list(&Filter {
            tag: Some("errand".into()),
            ..Default::default()
        });
        assert_eq!(errands.len(), 1);
    }

    #[test]
    fn list_sorts_by_priority_desc() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        sample(&mut store);
        let all = store.list(&Filter::default());
        assert_eq!(all[0].priority, Priority::High);
    }

    #[test]
    fn set_status_toggles_completed_at() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let t = store.add(NewTodo {
            title: "task".into(),
            ..Default::default()
        });

        let done = store.set_status(t.id, Status::Done).unwrap();
        assert_eq!(done.status, Status::Done);
        assert!(done.completed_at.is_some());

        let reopened = store.set_status(t.id, Status::Open).unwrap();
        assert_eq!(reopened.status, Status::Open);
        assert!(reopened.completed_at.is_none());
    }

    #[test]
    fn update_adds_and_removes_tags_without_duplicates() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let t = store.add(NewTodo {
            title: "task".into(),
            tags: vec!["a".into(), "b".into()],
            ..Default::default()
        });

        let updated = store
            .update(
                t.id,
                TodoPatch {
                    add_tags: vec!["a".into(), "c".into()], // "a" already present
                    remove_tags: vec!["b".into()],
                    project: Some(Some("newproj".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.tags, vec!["a", "c"]);
        assert_eq!(updated.project.as_deref(), Some("newproj"));
    }

    #[test]
    fn delete_and_notfound() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let t = store.add(NewTodo {
            title: "task".into(),
            ..Default::default()
        });
        store.delete(t.id).unwrap();
        assert!(store.get(t.id).is_none());
        assert!(matches!(store.delete(t.id), Err(Error::NotFound(_))));
    }

    #[test]
    fn save_then_load_roundtrips_and_is_atomic() {
        let dir = std::env::temp_dir().join(format!("agentodo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("todos.json");

        let mut store = Store::load(&path).unwrap();
        sample(&mut store);
        store.save().unwrap();

        // Save again over the existing file to exercise the temp-file rename path.
        store.add(NewTodo {
            title: "third".into(),
            ..Default::default()
        });
        store.save().unwrap();

        let reloaded = Store::load(&path).unwrap();
        assert_eq!(reloaded.all().len(), 3);
        // next_id must survive the round-trip so ids stay unique.
        let next = reloaded.all().iter().map(|t| t.id).max().unwrap();
        assert_eq!(next, 3);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn projects_lists_distinct_names_alphabetically_with_unassigned_last() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        sample(&mut store); // home, work
        store.add(NewTodo {
            title: "no project".into(),
            ..Default::default()
        });
        store.add(NewTodo {
            title: "another home task".into(),
            project: Some("home".into()),
            ..Default::default()
        });

        let projects = store.projects();
        let names: Vec<Option<&str>> = projects.iter().map(|p| p.project.as_deref()).collect();
        assert_eq!(names, vec![Some("home"), Some("work"), None]);
        assert_eq!(projects[0].open, 2);
        assert_eq!(projects[2].open, 1);
    }

    #[test]
    fn projects_splits_open_and_done_counts() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let a = store.add(NewTodo {
            title: "shipped".into(),
            project: Some("work".into()),
            ..Default::default()
        });
        store.add(NewTodo {
            title: "still going".into(),
            project: Some("work".into()),
            ..Default::default()
        });
        store.set_status(a.id, Status::Done).unwrap();

        let projects = store.projects();
        assert_eq!(projects[0].project.as_deref(), Some("work"));
        assert_eq!((projects[0].open, projects[0].done), (1, 1));
    }

    #[test]
    fn projects_is_empty_for_an_empty_store() {
        let store = Store::load("/nonexistent/ignored.json").unwrap();
        assert!(store.projects().is_empty());
    }

    #[test]
    fn projects_reports_unassigned_even_when_every_todo_has_a_project() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        sample(&mut store);

        let projects = store.projects();
        let last = projects.last().unwrap();
        assert_eq!(last.project, None);
        assert_eq!((last.open, last.done), (0, 0));
    }

    #[test]
    fn projects_folds_an_empty_project_string_into_unassigned() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        store.add(NewTodo {
            title: "hand-edited".into(),
            project: Some(String::new()),
            ..Default::default()
        });

        let projects = store.projects();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].project, None);
        assert_eq!(projects[0].open, 1);
    }

    #[test]
    fn projects_are_case_sensitive_so_typos_stay_visible() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        for name in ["work", "Work"] {
            store.add(NewTodo {
                title: name.into(),
                project: Some(name.into()),
                ..Default::default()
            });
        }

        let projects = store.projects();
        let names: Vec<Option<&str>> = projects.iter().map(|p| p.project.as_deref()).collect();
        assert_eq!(names, vec![Some("Work"), Some("work"), None]);
    }

    #[test]
    fn restore_keeps_original_id_and_rejects_duplicates() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        sample(&mut store);
        let removed = store.delete(1).unwrap();

        let restored = store.restore(removed.clone()).unwrap();
        assert_eq!(restored, removed);
        assert_eq!(store.get(1), Some(&removed));
        assert!(matches!(store.restore(removed), Err(Error::IdTaken(1))));

        // Fresh adds never collide with a restored id.
        let next = store.add(NewTodo {
            title: "next".into(),
            ..Default::default()
        });
        assert_eq!(next.id, 3);
    }

    #[test]
    fn restore_bumps_next_id_past_the_restored_id() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let mut ghost = store.add(NewTodo::default());
        store.delete(ghost.id).unwrap();
        ghost.id = 10;
        store.restore(ghost).unwrap();
        assert_eq!(store.add(NewTodo::default()).id, 11);
    }

    #[test]
    fn replace_overwrites_verbatim_and_requires_existing_id() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        sample(&mut store);
        let before = store.get(1).unwrap().clone();
        store
            .update(
                1,
                TodoPatch {
                    title: Some("changed".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        store.replace(before.clone()).unwrap();
        assert_eq!(store.get(1), Some(&before));

        let mut missing = before;
        missing.id = 99;
        assert!(matches!(store.replace(missing), Err(Error::NotFound(99))));
    }

    #[test]
    fn resolve_path_prefers_explicit_flag() {
        let p = Store::resolve_path(Some("/explicit.json".into())).unwrap();
        assert_eq!(p, std::path::PathBuf::from("/explicit.json"));
    }

    fn temp_store_path(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("agentodo-log-{}-{}", name, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("todos.json")
    }

    #[test]
    fn every_mutation_is_logged_with_snapshots_and_actor() {
        let path = temp_store_path("mutations");
        let mut store = Store::load(&path).unwrap();
        store.set_actor(Actor {
            source: Source::Mcp,
            client: Some("claude-code".into()),
        });

        let t = store.add(NewTodo {
            title: "task".into(),
            ..Default::default()
        });
        store.set_status(t.id, Status::Done).unwrap();
        store
            .update(
                t.id,
                TodoPatch {
                    title: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let removed = store.delete(t.id).unwrap();
        store.restore(removed.clone()).unwrap();
        store.replace(t.clone()).unwrap();
        store.save().unwrap();

        let log = read_log(&store.log_path()).unwrap();
        let ops: Vec<&str> = log.iter().map(|e| e.op.as_str()).collect();
        assert_eq!(
            ops,
            [
                "add",
                "set_status",
                "update",
                "delete",
                "restore",
                "replace"
            ]
        );
        assert!(log.iter().all(|e| e.id == t.id));
        assert!(log
            .iter()
            .all(|e| e.actor.client.as_deref() == Some("claude-code")));

        assert_eq!(log[0].kind(), EntryKind::Created);
        assert_eq!(log[0].after.as_ref(), Some(&t));
        assert_eq!(log[3].kind(), EntryKind::Deleted);
        assert_eq!(log[3].before.as_ref(), Some(&removed));
        assert_eq!(log[5].after.as_ref(), Some(&t));

        let diff = log[1].diff();
        assert_eq!(diff.len(), 1);
        assert_eq!(
            (diff[0].field, diff[0].old.as_str(), diff[0].new.as_str()),
            ("status", "open", "done")
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn save_appends_once_and_skips_no_op_updates() {
        let path = temp_store_path("append");
        let mut store = Store::load(&path).unwrap();
        let t = store.add(NewTodo::default());
        store.save().unwrap();
        store.save().unwrap(); // nothing pending: nothing appended

        store.update(t.id, TodoPatch::default()).unwrap();
        store.save().unwrap();

        let mut reloaded = Store::load(&path).unwrap();
        reloaded.delete(t.id).unwrap();
        reloaded.save().unwrap();

        let log = read_log(&store.log_path()).unwrap();
        let ops: Vec<&str> = log.iter().map(|e| e.op.as_str()).collect();
        assert_eq!(ops, ["add", "delete"]);
        assert_eq!(log[0].actor.source, Source::Unknown);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn read_log_tails_from_offset_and_skips_garbage() {
        let path = temp_store_path("tail");
        let log = log_path(&path);
        assert!(read_log(&log).unwrap().is_empty(), "missing log is empty");

        let mut store = Store::load(&path).unwrap();
        store.add(NewTodo::default());
        store.save().unwrap();
        let (first, offset) = read_log_from(&log, 0).unwrap();
        assert_eq!(first.len(), 1);

        // Garbage and a half-written line must not break or be consumed.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(b"not json\n{\"partial").unwrap();
        drop(f);
        store.add(NewTodo::default());

        let (next, offset2) = read_log_from(&log, offset).unwrap();
        assert!(next.is_empty());
        let len = std::fs::metadata(&log).unwrap().len();
        assert_eq!(offset2, len - "{\"partial".len() as u64);

        // A log shorter than the offset is re-read from the start.
        std::fs::write(&log, "").unwrap();
        store.save().unwrap();
        let (again, _) = read_log_from(&log, offset2).unwrap();
        assert_eq!(again.len(), 1);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn log_path_sits_next_to_the_store() {
        assert_eq!(
            log_path(std::path::Path::new("/data/todos.json")),
            std::path::PathBuf::from("/data/todos.log.jsonl")
        );
    }
}
