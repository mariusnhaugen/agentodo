//! Core domain and storage for agentodo.
//!
//! This crate holds all todo logic behind a small API ([`Store`]) so that the
//! CLI and the MCP server are thin adapters over the same functions.

mod error;
mod filter;
mod model;
mod store;

pub use error::{Error, Result};
pub use filter::{Filter, SortBy};
pub use model::{NewTodo, Priority, Status, Todo, TodoPatch};
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
}
