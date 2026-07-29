# agentodo

A small todo tracker you can drive from the command line and expose to an agent
over MCP. The todo logic lives in one core library; the CLI and the MCP server
are thin adapters over it.

## Layout

```
crates/
  core/   agentodo-core — domain model + JSON storage (all the real logic)
  cli/    agentodo      — command-line interface
  mcp/    agentodo-mcp  — MCP server (stdio) exposing todos to an agent
```

## Storage

Todos live in a single JSON file. Resolution order:

1. `--store <path>` (CLI only)
2. `$AGENTODO_STORE`
3. platform default (`~/.local/share/agentodo/todos.json` on Linux)

Writes are atomic (temp file + rename), so an interrupted write can't corrupt the
store.

## CLI

```sh
cargo build --release        # binary at target/release/agentodo

agentodo add "ship release" --priority high --project work --tag urgent
agentodo add "buy milk" -p low -t home -n "oat, not almond"

agentodo list                          # open todos, highest priority first
agentodo list --status all
agentodo list --project work
agentodo list --tag urgent --sort created

agentodo show 1
agentodo done 1
agentodo undone 1
agentodo edit 2 --priority high --add-tag blocked --project ""   # "" clears project
agentodo rm 2

agentodo list --json         # machine-readable output (global flag)
```

## MCP server

`agentodo-mcp` speaks the MCP stdio transport (newline-delimited JSON-RPC 2.0) and
exposes six tools, each mapping to one core operation:

`add_todo`, `list_todos`, `get_todo`, `complete_todo`, `update_todo`, `delete_todo`.

Register it with an MCP client. For Claude Code / Claude Desktop, add to the
`mcpServers` block:

```json
{
  "mcpServers": {
    "agentodo": {
      "command": "/absolute/path/to/target/release/agentodo-mcp",
      "env": {
        "AGENTODO_STORE": "/home/you/.local/share/agentodo/todos.json"
      }
    }
  }
}
```

The CLI and the MCP server share the same store file, so todos you add on the
command line are visible to the agent and vice versa.

## Development

```sh
cargo test --workspace      # core has unit tests for CRUD, filtering, round-trip
cargo clippy --workspace
```

## Not in v1 (deferred by design)

Markdown / Obsidian export, due dates, and subtasks are intentionally out of scope.
The core store is structured so these slot in without reworking the CLI or MCP
layers — e.g. an `export` command would render the store to markdown while JSON
remains the source of truth.
