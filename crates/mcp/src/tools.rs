//! Tool definitions and handlers. Each tool is a thin adapter that loads the
//! store, calls one core function, saves, and returns the affected todo(s) as
//! JSON text.

use std::sync::OnceLock;

use agentodo_core::{
    parse_day, parse_duration, parse_scheduled_filter, Actor, Filter, NewTodo, Priority, SortBy,
    Source, Status, Store, TodoPatch,
};
use chrono::{Local, NaiveDate, Utc};
use serde_json::{json, Value};

/// The result of invoking a tool: text to return to the caller, and whether it
/// represents an error the model should see.
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    fn ok(value: Value) -> Self {
        ToolResult {
            text: serde_json::to_string_pretty(&value).unwrap_or_else(|e| e.to_string()),
            is_error: false,
        }
    }

    fn err(message: impl Into<String>) -> Self {
        ToolResult {
            text: message.into(),
            is_error: true,
        }
    }
}

/// The connected client's `clientInfo.name`, captured at `initialize`.
static CLIENT_NAME: OnceLock<String> = OnceLock::new();

/// Records which MCP client is connected so its changes are attributed to it
/// in the change log. Only the first call counts.
pub fn set_client_name(name: &str) {
    CLIENT_NAME.set(name.to_owned()).ok();
}

fn load() -> Result<Store, String> {
    let path = Store::resolve_path(None).map_err(|e| e.to_string())?;
    let mut store = Store::load(&path).map_err(|e| e.to_string())?;
    // `$AGENTODO_ACTOR` wins over what the client calls itself.
    let mut actor = Actor::from_env(Source::Mcp);
    actor.client = actor.client.or_else(|| CLIENT_NAME.get().cloned());
    store.set_actor(actor);
    Ok(store)
}

/// The list of tools advertised to the client, with JSON Schema for inputs.
pub fn definitions() -> Value {
    let priority_enum = json!({ "type": "string", "enum": ["low", "medium", "high"] });
    json!([
        {
            "name": "add_todo",
            "description": "Create a new todo. Returns the created todo.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "What needs doing." },
                    "priority": priority_enum,
                    "project": { "type": "string", "description": "Optional project the todo belongs to." },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "notes": { "type": "string" },
                    "scheduled": { "type": "string", "description": "Day to work on it (a soft plan, not a deadline): today, tomorrow, mon…sun (next occurrence) or YYYY-MM-DD." },
                    "plan": { "type": "string", "description": "Time budgeted for the whole todo, across sessions: 30m, 2h, 1h30m (bare numbers are minutes)." }
                },
                "required": ["title"]
            }
        },
        {
            "name": "list_todos",
            "description": "List todos, optionally filtered by status, priority, tag, project, or scheduled day.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["open", "done", "all"], "description": "Defaults to open." },
                    "priority": priority_enum,
                    "tag": { "type": "string" },
                    "project": { "type": "string" },
                    "scheduled": { "type": "string", "description": "today, tomorrow, mon…sun, YYYY-MM-DD, week (this Monday–Sunday), or none (unscheduled)." },
                    "sort": { "type": "string", "enum": ["priority", "created"], "description": "Defaults to priority." }
                }
            }
        },
        {
            "name": "get_todo",
            "description": "Fetch a single todo by id, including its focus sessions, total time_spent_secs, and plan_secs (time budgeted) if set.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "integer" } },
                "required": ["id"]
            }
        },
        {
            "name": "complete_todo",
            "description": "Mark a todo done, or reopen it with done=false.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "done": { "type": "boolean", "description": "Defaults to true." }
                },
                "required": ["id"]
            }
        },
        {
            "name": "update_todo",
            "description": "Edit fields of an existing todo. Omitted fields are unchanged. Pass an empty string for project/notes/scheduled/plan to clear them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "title": { "type": "string" },
                    "priority": priority_enum,
                    "project": { "type": "string" },
                    "notes": { "type": "string" },
                    "scheduled": { "type": "string", "description": "Day to work on it: today, tomorrow, mon…sun or YYYY-MM-DD. Empty string or none unschedules it." },
                    "plan": { "type": "string", "description": "Time budgeted for the whole todo: 30m, 2h, 1h30m. Empty string or none clears it." },
                    "add_tags": { "type": "array", "items": { "type": "string" } },
                    "remove_tags": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["id"]
            }
        },
        {
            "name": "delete_todo",
            "description": "Remove a todo by id. Returns the deleted todo.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "integer" } },
                "required": ["id"]
            }
        },
        {
            "name": "list_projects",
            "description": "List distinct projects with open/done todo counts. Unassigned todos are reported as a null project.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "get_timer",
            "description": "The user's running focus timer: which todo they're working on, how long is left in the planned block, and how far past it they've gone (overtime_secs). While paused (paused: true) the clock stands still and ends_at is null. The timer keeps running until the user stops it. Returns null when no timer is running. Read-only; the user starts and stops timers themselves.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }
    ])
}

/// Dispatch a `tools/call` to the named tool.
pub fn call(name: &str, args: &Value) -> ToolResult {
    let result = match name {
        "add_todo" => add_todo(args),
        "list_todos" => list_todos(args),
        "get_todo" => get_todo(args),
        "complete_todo" => complete_todo(args),
        "update_todo" => update_todo(args),
        "delete_todo" => delete_todo(args),
        "list_projects" => list_projects(args),
        "get_timer" => get_timer(args),
        other => Err(format!("unknown tool: {other}")),
    };
    match result {
        Ok(value) => ToolResult::ok(value),
        Err(message) => ToolResult::err(message),
    }
}

// --- argument helpers -------------------------------------------------------

fn require_id(args: &Value) -> Result<u64, String> {
    args.get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| "missing or invalid 'id'".to_string())
}

fn opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

fn opt_priority(args: &Value) -> Result<Option<Priority>, String> {
    match args.get("priority").and_then(Value::as_str) {
        None => Ok(None),
        Some("low") => Ok(Some(Priority::Low)),
        Some("medium") => Ok(Some(Priority::Medium)),
        Some("high") => Ok(Some(Priority::High)),
        Some(other) => Err(format!("invalid priority: {other}")),
    }
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn day(s: &str) -> Result<NaiveDate, String> {
    parse_day(s, today()).ok_or_else(|| format!("invalid scheduled day: {s}"))
}

// --- tool implementations ---------------------------------------------------

fn plan(s: &str) -> Result<chrono::Duration, String> {
    parse_duration(s).ok_or_else(|| format!("invalid plan {s:?}: try 30m, 2h or 1h30m"))
}

fn add_todo(args: &Value) -> Result<Value, String> {
    let title = opt_str(args, "title").ok_or("missing 'title'")?;
    let mut store = load()?;
    let todo = store.add(NewTodo {
        title,
        priority: opt_priority(args)?.unwrap_or_default(),
        project: opt_str(args, "project"),
        tags: string_array(args, "tags"),
        notes: opt_str(args, "notes"),
        scheduled: opt_str(args, "scheduled").as_deref().map(day).transpose()?,
        plan: opt_str(args, "plan").as_deref().map(plan).transpose()?,
    });
    store.save().map_err(|e| e.to_string())?;
    Ok(json!(todo))
}

fn list_todos(args: &Value) -> Result<Value, String> {
    let status = match args.get("status").and_then(Value::as_str) {
        None | Some("open") => Some(Status::Open),
        Some("done") => Some(Status::Done),
        Some("all") => None,
        Some(other) => return Err(format!("invalid status: {other}")),
    };
    let sort = match args.get("sort").and_then(Value::as_str) {
        None | Some("priority") => SortBy::Priority,
        Some("created") => SortBy::Created,
        Some(other) => return Err(format!("invalid sort: {other}")),
    };
    let filter = Filter {
        status,
        priority: opt_priority(args)?,
        tag: opt_str(args, "tag"),
        project: opt_str(args, "project"),
        scheduled: opt_str(args, "scheduled")
            .map(|s| {
                parse_scheduled_filter(&s, today())
                    .ok_or_else(|| format!("invalid scheduled filter: {s}"))
            })
            .transpose()?,
        sort,
    };
    let store = load()?;
    Ok(json!(store.list(&filter)))
}

fn list_projects(_args: &Value) -> Result<Value, String> {
    let store = load()?;
    Ok(json!(store.projects()))
}

fn get_todo(args: &Value) -> Result<Value, String> {
    let id = require_id(args)?;
    let store = load()?;
    match store.get(id) {
        Some(todo) => {
            let mut value = json!(todo);
            value["time_spent_secs"] = json!(todo.time_spent().num_seconds());
            Ok(value)
        }
        None => Err(format!("no todo with id {id}")),
    }
}

fn get_timer(_args: &Value) -> Result<Value, String> {
    let store = load()?;
    let Some(active) = store.active_timer() else {
        return Ok(Value::Null);
    };
    let now = Utc::now();
    Ok(json!({
        "todo": active.todo,
        "title": store.get(active.todo).map(|t| t.title.clone()),
        "started_at": active.started_at,
        "planned_secs": active.planned_secs,
        "ends_at": active.ends_at(now),
        "elapsed_secs": active.elapsed(now).num_seconds(),
        "remaining_secs": active.remaining(now).num_seconds(),
        "overtime_secs": active.overtime(now).num_seconds(),
        "paused": active.is_paused(),
        "paused_at": active.paused_at,
    }))
}

fn complete_todo(args: &Value) -> Result<Value, String> {
    let id = require_id(args)?;
    let done = args.get("done").and_then(Value::as_bool).unwrap_or(true);
    let status = if done { Status::Done } else { Status::Open };
    let mut store = load()?;
    let todo = store.set_status(id, status).map_err(|e| e.to_string())?;
    store.save().map_err(|e| e.to_string())?;
    Ok(json!(todo))
}

fn update_todo(args: &Value) -> Result<Value, String> {
    let id = require_id(args)?;
    let patch = TodoPatch {
        title: opt_str(args, "title"),
        priority: opt_priority(args)?,
        // Empty string clears the field; absence leaves it unchanged.
        project: opt_str(args, "project").map(|p| if p.is_empty() { None } else { Some(p) }),
        notes: opt_str(args, "notes").map(|n| if n.is_empty() { None } else { Some(n) }),
        add_tags: string_array(args, "add_tags"),
        remove_tags: string_array(args, "remove_tags"),
        scheduled: match opt_str(args, "scheduled").as_deref() {
            None => None,
            Some("" | "none") => Some(None),
            Some(s) => Some(Some(day(s)?)),
        },
        plan: match opt_str(args, "plan").as_deref() {
            None => None,
            Some("" | "none") => Some(None),
            Some(s) => Some(Some(plan(s)?)),
        },
    };
    let mut store = load()?;
    let todo = store.update(id, patch).map_err(|e| e.to_string())?;
    store.save().map_err(|e| e.to_string())?;
    Ok(json!(todo))
}

fn delete_todo(args: &Value) -> Result<Value, String> {
    let id = require_id(args)?;
    let mut store = load()?;
    let todo = store.delete(id).map_err(|e| e.to_string())?;
    store.save().map_err(|e| e.to_string())?;
    Ok(json!(todo))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `definitions()` and `call()` are a hand-rolled two-place registry, so a
    /// tool can be advertised to clients yet never dispatched. Empty arguments
    /// make the mutating tools fail on their missing `required` field long
    /// before they reach `save()`, so nothing is written.
    #[test]
    fn every_declared_tool_is_dispatched() {
        // Sole test in this crate, so setting a process-global env var is safe.
        // If a second one is added, serialize them behind a shared fixture.
        std::env::set_var(
            "AGENTODO_STORE",
            std::env::temp_dir().join("agentodo-wiring-test.json"),
        );

        let definitions = definitions();
        let tools = definitions.as_array().expect("definitions is an array");
        assert!(!tools.is_empty());
        for tool in tools {
            let name = tool["name"].as_str().expect("tool has a name");
            let result = call(name, &json!({}));
            assert!(
                !result.text.starts_with("unknown tool:"),
                "{name} is declared but not dispatched"
            );
        }
    }
}
