//! Core domain and storage for agentodo.
//!
//! This crate holds all todo logic behind a small API ([`Store`]) so that the
//! CLI and the MCP server are thin adapters over the same functions.

mod date;
mod error;
mod filter;
mod log;
mod model;
mod parse;
mod store;
mod timer;

pub use date::{parse_day, parse_scheduled_filter, week_start, ScheduledFilter};
pub use error::{Error, Result};
pub use filter::{Filter, SortBy};
pub use log::{log_path, read_log, read_log_from, Actor, EntryKind, FieldChange, LogEntry, Source};
pub use model::{
    ActiveSession, NewTodo, Priority, ProjectSummary, Session, Status, Todo, TodoPatch,
};
pub use parse::{parse_quick_add, QuickAdd};
pub use store::Store;
pub use timer::{default_timer, format_countdown, format_duration, format_timer, parse_duration};

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
            scheduled: None,
        });
        store.add(NewTodo {
            title: "ship release".into(),
            priority: Priority::High,
            project: Some("work".into()),
            tags: vec!["urgent".into()],
            notes: Some("cut the tag first".into()),
            scheduled: None,
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

    fn ymd(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn scheduled_is_set_cleared_and_diffed() {
        let path = temp_store_path("scheduled");
        let mut store = Store::load(&path).unwrap();
        let t = store.add(NewTodo {
            title: "plan".into(),
            scheduled: Some(ymd(2026, 9, 25)),
            ..Default::default()
        });
        assert_eq!(t.scheduled, Some(ymd(2026, 9, 25)));

        let moved = store
            .update(
                t.id,
                TodoPatch {
                    scheduled: Some(Some(ymd(2026, 9, 28))),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(moved.scheduled, Some(ymd(2026, 9, 28)));
        let untouched = store.update(t.id, TodoPatch::default()).unwrap();
        assert_eq!(untouched.scheduled, Some(ymd(2026, 9, 28)));
        let cleared = store
            .update(
                t.id,
                TodoPatch {
                    scheduled: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared.scheduled, None);
        store.save().unwrap();

        let log = read_log(&store.log_path()).unwrap();
        let diff = log[1].diff();
        assert_eq!(
            (diff[0].field, diff[0].old.as_str(), diff[0].new.as_str()),
            ("scheduled", "2026-09-25", "2026-09-28")
        );
        assert_eq!(log[2].diff()[0].new, ""); // the no-op update is not logged

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn todos_without_scheduled_still_load_and_stay_compact() {
        let t = Store::load("/nonexistent/ignored.json")
            .unwrap()
            .add(NewTodo::default());
        let json = serde_json::to_string(&t).unwrap();
        assert!(!json.contains("scheduled"));
        let back: Todo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.scheduled, None);
    }

    #[test]
    fn parse_day_understands_relative_words() {
        let fri = ymd(2026, 9, 25);
        assert_eq!(parse_day("today", fri), Some(fri));
        assert_eq!(parse_day("Tomorrow", fri), Some(ymd(2026, 9, 26)));
        assert_eq!(parse_day("fri", fri), Some(fri)); // today counts
        assert_eq!(parse_day("thursday", fri), Some(ymd(2026, 10, 1)));
        assert_eq!(parse_day("mon", fri), Some(ymd(2026, 9, 28)));
        assert_eq!(parse_day("2026-12-24", fri), Some(ymd(2026, 12, 24)));
        assert_eq!(parse_day("someday", fri), None);
    }

    #[test]
    fn week_start_is_monday_even_from_sunday() {
        assert_eq!(week_start(ymd(2026, 9, 21)), ymd(2026, 9, 21));
        assert_eq!(week_start(ymd(2026, 9, 25)), ymd(2026, 9, 21));
        assert_eq!(week_start(ymd(2026, 9, 27)), ymd(2026, 9, 21));
        assert_eq!(week_start(ymd(2026, 9, 28)), ymd(2026, 9, 28));
    }

    #[test]
    fn filter_by_scheduled() {
        let fri = ymd(2026, 9, 25);
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        for (title, day) in [
            ("none", None),
            ("mon", Some(ymd(2026, 9, 21))),
            ("fri", Some(fri)),
            ("next", Some(ymd(2026, 9, 28))),
        ] {
            store.add(NewTodo {
                title: title.into(),
                scheduled: day,
                ..Default::default()
            });
        }
        let titles = |f: &str| {
            let mut v: Vec<String> = store
                .list(&Filter {
                    scheduled: parse_scheduled_filter(f, fri),
                    ..Default::default()
                })
                .into_iter()
                .map(|t| t.title)
                .collect();
            v.sort();
            v
        };
        assert_eq!(titles("none"), ["none"]);
        assert_eq!(titles("today"), ["fri"]);
        assert_eq!(titles("week"), ["fri", "mon"]);
        assert_eq!(titles("2026-09-28"), ["next"]);
    }

    fn at(min: i64) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(2026, 10, 5, 9, 0, 0).unwrap() + chrono::Duration::minutes(min)
    }

    fn mins(n: i64) -> chrono::Duration {
        chrono::Duration::minutes(n)
    }

    #[test]
    fn stopping_records_the_time_including_overtime() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let t = store.add(NewTodo::default());
        let active = store.start_timer(t.id, mins(25), at(0)).unwrap();
        assert_eq!(active.remaining(at(7)), mins(18));
        assert!(!active.is_over(at(24)));
        assert_eq!(active.overtime(at(40)), mins(15));
        assert_eq!(store.active_timer(), Some(&active));

        let early = store.stop_timer(at(10)).unwrap();
        assert_eq!(early.time_spent(), mins(10));
        assert_eq!(early.sessions[0].overtime(), None);
        assert_eq!(store.active_timer(), None);
        assert_eq!(store.stop_timer(at(11)), None, "nothing left to stop");

        // A forgotten timer runs on until stopped: nothing closes it.
        store.start_timer(t.id, mins(25), at(100)).unwrap();
        let late = store.stop_timer(at(100 + 25 + 565)).unwrap();
        assert_eq!(late.sessions[1].duration(), mins(590));
        assert_eq!(late.sessions[1].overtime(), Some(mins(565)));
    }

    #[test]
    fn recorded_sessions_can_be_shortened_deleted_and_added() {
        let path = temp_store_path("edit-sessions");
        let mut store = Store::load(&path).unwrap();
        let t = store.add(NewTodo::default());
        store.start_timer(t.id, mins(25), at(0)).unwrap();
        store.stop_timer(at(590)).unwrap();

        let fixed = store.set_session_length(t.id, 0, mins(29)).unwrap();
        assert_eq!(fixed.sessions[0].start, at(0));
        assert_eq!(fixed.sessions[0].end, at(29));
        assert_eq!(fixed.sessions[0].overtime(), Some(mins(4)));

        let added = store.add_session(t.id, mins(45), at(1000)).unwrap();
        assert_eq!(added.sessions[1].start, at(955));
        assert_eq!(added.sessions[1].overtime(), None, "no block, no overtime");
        assert_eq!(added.time_spent(), mins(74));

        let deleted = store.delete_session(t.id, 0).unwrap();
        assert_eq!(deleted.time_spent(), mins(45));
        assert!(matches!(
            store.delete_session(t.id, 5),
            Err(Error::NoSession { id: 1, index: 5 })
        ));
        assert!(matches!(
            store.add_session(9, mins(5), at(0)),
            Err(Error::NotFound(9))
        ));

        let restored = store.set_sessions(t.id, fixed.sessions.clone()).unwrap();
        assert_eq!(restored.sessions, fixed.sessions);
        store.save().unwrap();

        let log = read_log(&store.log_path()).unwrap();
        let ops: Vec<&str> = log.iter().map(|e| e.op.as_str()).collect();
        assert_eq!(
            ops,
            ["add", "session", "session", "session", "session", "session"]
        );
        let diff = log[2].diff();
        assert_eq!(
            (diff[0].field, diff[0].old.as_str(), diff[0].new.as_str()),
            ("time", "9h50m", "29m")
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn starting_another_timer_switches_and_records_the_first() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let a = store.add(NewTodo::default());
        let b = store.add(NewTodo::default());
        store.start_timer(a.id, mins(25), at(0)).unwrap();
        store.start_timer(b.id, mins(50), at(5)).unwrap();

        assert_eq!(store.get(a.id).unwrap().time_spent(), mins(5));
        assert_eq!(store.active_timer().unwrap().todo, b.id);
        assert!(matches!(
            store.start_timer(99, mins(5), at(6)),
            Err(Error::NotFound(99))
        ));
        assert_eq!(
            store.active_timer().unwrap().todo,
            b.id,
            "failed start keeps the timer"
        );

        // A restart at the same instant records nothing.
        store.start_timer(b.id, mins(50), at(5)).unwrap();
        assert!(store.get(b.id).unwrap().sessions.is_empty());
    }

    #[test]
    fn done_stops_the_timer_and_delete_drops_it() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let a = store.add(NewTodo::default());
        let b = store.add(NewTodo::default());

        let started = chrono::Utc::now() - mins(40);
        store.start_timer(b.id, mins(25), started).unwrap();
        store.set_status(a.id, Status::Done).unwrap();
        assert!(store.active_timer().is_some(), "other todos don't stop it");
        let done = store.set_status(b.id, Status::Done).unwrap();
        assert!(done.time_spent() >= mins(40), "overtime counts");
        assert_eq!(store.active_timer(), None);

        store.start_timer(a.id, mins(25), at(0)).unwrap();
        let removed = store.delete(a.id).unwrap();
        assert!(removed.sessions.is_empty());
        assert_eq!(store.active_timer(), None);
    }

    #[test]
    fn replace_keeps_sessions_so_undo_never_loses_time() {
        let mut store = Store::load("/nonexistent/ignored.json").unwrap();
        let t = store.add(NewTodo::default());
        store.start_timer(t.id, mins(25), at(0)).unwrap();
        store.stop_timer(at(25)).unwrap();

        let restored = store.replace(t.clone()).unwrap();
        assert_eq!(restored.time_spent(), mins(25));
    }

    #[test]
    fn the_running_timer_survives_a_save_and_old_stores_still_load() {
        let path = temp_store_path("active");
        let mut store = Store::load(&path).unwrap();
        let t = store.add(NewTodo::default());
        store.save().unwrap();
        let json = std::fs::read_to_string(&path).unwrap();
        assert!(!json.contains("active_session") && !json.contains("sessions"));

        let active = store.start_timer(t.id, mins(25), at(0)).unwrap();
        store.save().unwrap();
        assert_eq!(Store::load(&path).unwrap().active_timer(), Some(&active));

        std::fs::write(&path, r#"{"next_id": 1, "todos": []}"#).unwrap();
        assert_eq!(Store::load(&path).unwrap().active_timer(), None);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
