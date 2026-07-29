//! Tool definitions and handlers. Each tool is a thin adapter that loads the
//! store, calls one core function, saves, and returns the affected todo(s) as
//! JSON text.

use std::path::PathBuf;

use agentodo_core::{Filter, NewTodo, Priority, SortBy, Status, Store, TodoPatch};
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

/// Store path precedence: `$AGENTODO_STORE`, then the platform default.
fn store_path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("AGENTODO_STORE") {
        return Ok(PathBuf::from(p));
    }
    Store::default_path().map_err(|e| e.to_string())
}

fn load() -> Result<Store, String> {
    let path = store_path()?;
    Store::load(&path).map_err(|e| e.to_string())
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
                    "notes": { "type": "string" }
                },
                "required": ["title"]
            }
        },
        {
            "name": "list_todos",
            "description": "List todos, optionally filtered by status, priority, tag, or project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "status": { "type": "string", "enum": ["open", "done", "all"], "description": "Defaults to open." },
                    "priority": priority_enum,
                    "tag": { "type": "string" },
                    "project": { "type": "string" },
                    "sort": { "type": "string", "enum": ["priority", "created"], "description": "Defaults to priority." }
                }
            }
        },
        {
            "name": "get_todo",
            "description": "Fetch a single todo by id.",
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
            "description": "Edit fields of an existing todo. Omitted fields are unchanged. Pass an empty string for project/notes to clear them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer" },
                    "title": { "type": "string" },
                    "priority": priority_enum,
                    "project": { "type": "string" },
                    "notes": { "type": "string" },
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
    args.get(key)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
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

// --- tool implementations ---------------------------------------------------

fn add_todo(args: &Value) -> Result<Value, String> {
    let title = opt_str(args, "title").ok_or("missing 'title'")?;
    let mut store = load()?;
    let todo = store.add(NewTodo {
        title,
        priority: opt_priority(args)?.unwrap_or_default(),
        project: opt_str(args, "project"),
        tags: string_array(args, "tags"),
        notes: opt_str(args, "notes"),
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
        sort,
    };
    let store = load()?;
    Ok(json!(store.list(&filter)))
}

fn get_todo(args: &Value) -> Result<Value, String> {
    let id = require_id(args)?;
    let store = load()?;
    match store.get(id) {
        Some(todo) => Ok(json!(todo)),
        None => Err(format!("no todo with id {id}")),
    }
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
