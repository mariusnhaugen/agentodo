# agentodo

A small todo tracker you can drive from the command line or a vim-style TUI, and
expose to an agent over MCP. The todo logic lives in one core library; the CLI,
TUI and MCP server are thin adapters over it.

## Layout

```
crates/
  core/   agentodo-core — domain model + JSON storage (all the real logic)
  cli/    agentodo      — command-line interface
  mcp/    agentodo-mcp  — MCP server (stdio) exposing todos to an agent
  tui/    agentodo-tui  — vim-style terminal UI
```

## Storage

Todos live in a single JSON file. Resolution order:

1. `--store <path>` (CLI and TUI)
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

agentodo projects            # distinct projects with open/done counts

agentodo list --json         # machine-readable output (global flag)
```

## TUI

```sh
agentodo-tui                 # or: agentodo-tui --store <path>
```

A flat list with the current line highlighted, driven by vim keys. Press `?` in
the app for the full list.

| Keys | Action |
|---|---|
| `j` `k` (with counts), `gg` `G`, `C-d` `C-u` | move |
| `o` `O` | add below / above — `title !h +project #tag` |
| `i`, `cc` | edit / retype title |
| `dd`, `3dd` | delete |
| `x`, `+` `-`, `gp`, `e` | toggle done, priority, project, notes in `$EDITOR` |
| `u`, `C-r` | undo / redo (deleted todos come back with the same id) |
| `/` `n` `N` | search titles |
| `p`, `.`, `s` | project filter, show/hide done, toggle sort |
| `:q` `:project [name]` `:sort priority\|created` `:hide` `:show` | commands |

Every change is written immediately (reload → edit → save), and the store file is
polled every second, so todos an agent adds over MCP appear while you watch
without either side overwriting the other.

## MCP server

`agentodo-mcp` speaks the MCP stdio transport (newline-delimited JSON-RPC 2.0) and
exposes seven tools, each mapping to one core operation:

`add_todo`, `list_todos`, `get_todo`, `complete_todo`, `update_todo`, `delete_todo`,
`list_projects`.

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

The CLI, TUI and MCP server share the same store file, so todos you add on the
command line or in the TUI are visible to the agent and vice versa.

## Development

```sh
cargo test --workspace      # core CRUD/parsing tests, TUI keymap + render tests
cargo clippy --workspace
```

## Not in v1 (deferred by design)

Markdown / Obsidian export, due dates, and subtasks are intentionally out of scope.
The core store is structured so these slot in without reworking the CLI or MCP
layers — e.g. an `export` command would render the store to markdown while JSON
remains the source of truth.
