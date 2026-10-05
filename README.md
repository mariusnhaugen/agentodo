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

## Install

```sh
cargo install --locked --path crates/cli   # agentodo
cargo install --locked --path crates/tui   # agentodo-tui
cargo install --locked --path crates/mcp   # agentodo-mcp
```

This copies the binaries into `~/.cargo/bin`, so they stay stable while you hack
on the repo. Re-run the same commands (add `--force` if cargo refuses) to pick up
new changes. `--locked` matters: without it `cargo install` ignores `Cargo.lock`
and may pull dependency versions that need a newer rustc.

## CLI

```sh
cargo build --release        # dev build at target/release/agentodo

agentodo add "ship release" --priority high --project work --tag urgent
agentodo add "buy milk" -p low -t home -n "oat, not almond"
agentodo add "refactor store" --scheduled fri   # plan it for a day

agentodo list                          # open todos, highest priority first
agentodo list --status all
agentodo list --project work
agentodo list --tag urgent --sort created
agentodo list --scheduled week         # also: today, fri, 2026-10-02, none

agentodo show 1
agentodo done 1
agentodo undone 1
agentodo edit 2 --priority high --add-tag blocked --project ""   # "" clears project
agentodo edit 2 --scheduled none       # back to unscheduled
agentodo rm 2

agentodo projects            # distinct projects with open/done counts

agentodo start 1             # 25-minute focus timer on #1
agentodo start 1 50m         # or 50, 1h, 1h30m
agentodo timer               # what's running: time left, or +01:12 over
agentodo stop                # record the time, overtime included

agentodo time 1              # list #1's sessions, newest first
agentodo time 1 34m          # fix the newest one (left running?) to 34 minutes
agentodo time 1 20m -s 2     # fix the one before it
agentodo time 1 --rm -s 2    # delete it
agentodo time 1 --add 45m    # record time worked without the timer

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
| `]c` `[c`, `:log`, `:seen` | jump between changed todos, change log, clear markers |
| `T`, `:stop` | start a focus timer on this todo (asks how long) / stop it |
| `Enter`, then `c` `dd` `a` | details, then change / delete / add time on the selected session |
| `W`, `:week` `:list` | switch between the list and the week board |

### Week board

`W` opens a `Backlog | Mon … Sun` board for planning your week. Each todo can
have a **scheduled** day. It's a soft "do-on" date, not a deadline, so you can
move cards around freely. Backlog holds open todos with no day. An open todo
whose day has passed shows on today with `↻N` (days slipped), and its stored
date is left alone until you move it. Done todos stay on their day, dimmed.

| Keys | Action |
|---|---|
| `h` `l`, `j` `k` | move between days / within a day |
| `H` `L` (with counts) | shove a card a day earlier / later; past Sunday rolls into next week, before Monday is Backlog |
| `gt`, `gb` | schedule for today / send back to Backlog (also work in the list) |
| `gw` + `m t w r f s u` / `b` | send to that weekday of the week on screen / Backlog |
| `C-n` `C-p` | next / previous week |
| `o` `O` | add a todo on the focused column's day |

Everything else (`x`, `+` `-`, `dd`, `u`, `e`, `Enter`, `/`) works as in the list,
on the focused column.

Every change is written immediately (reload → edit → save), and the store file is
polled every second, so todos an agent adds over MCP appear while you watch
without either side overwriting the other.

## Focus timer

Start a timer on a todo with a planned block of time, 25 minutes by default.
When the block is up, the bell rings once and the timer keeps going,
counting overtime (`+01:12`) until you stop it. A session records how long
you really worked, so a task that runs over shows exactly how far over it
went.

One timer runs at a time. It lives in the store file, so it keeps going when
you quit the TUI, and the CLI, TUI and agents all see the same timer.

- **Stopping records the time**, overtime included. `T` or `:stop` stops it,
  so does marking the todo done, and starting a timer on another todo
  switches. Deleting the todo drops it.
- **Fix it afterwards.** If you walked away and the timer ran all night,
  correct the session instead of losing it. In the TUI, `Enter` on a todo
  lists its sessions newest first, with overtime called out. The newest
  session is selected; `c` takes a new length (`34m`), `dd` deletes, `a`
  adds time worked without the timer, and `u` undoes. From the shell:
  `agentodo time 3 34m`.
- **Undo never loses time.** `u` and `:log` revert restore everything about a
  todo except its sessions. Session edits have their own undo, and recorded
  sessions can't be reverted from `:log`.

While a timer runs, the TUI header shows `⏱ 18:42 #3 title`, and the todo's
row (or its card on the week board) counts down, then counts up in yellow
once it's in overtime. When the block is up, the TUI shows a banner and rings
the terminal bell. Windows Terminal turns the bell into a taskbar flash. To
get a desktop notification as well, set `$AGENTODO_TIMER_HOOK` to a shell
command. It runs with `$AGENTODO_TODO_ID` and `$AGENTODO_TODO_TITLE` set.
For example, a Windows toast from WSL:

```sh
export AGENTODO_TIMER_HOOK='powershell.exe -NoProfile -Command "New-BurntToastNotification -Text \"Block done\", \"$AGENTODO_TODO_TITLE\""'
```

The bell only rings from an open TUI, and only if the block ended in the
last minute. A TUI opened later just shows the overtime. Set
`$AGENTODO_TIMER_MINUTES` to change the default block (e.g. `30`).

## Change log

Every change made through the CLI, TUI or MCP server is appended to
`todos.log.jsonl` next to the store: one JSON line with a timestamp, who made it
(`cli`, `tui` or `mcp`, plus the MCP client's name), the operation, and full
before/after snapshots of the todo. Set `$AGENTODO_ACTOR` to label changes from
a particular script or agent.

In the TUI, todos changed by anyone else since your last session get a `•`
marker (`]c`/`[c` jump between them; moving past one clears it). `:log` shows
the history newest first: `Enter` jumps to the todo, `a` shows only others'
changes, and `r` reverts an entry, as long as the todo hasn't changed since.
A revert is an ordinary change, so `u` undoes it.

## MCP server

`agentodo-mcp` speaks the MCP stdio transport (newline-delimited JSON-RPC 2.0) and
exposes eight tools, each mapping to one core operation. `add_todo`,
`update_todo` and `list_todos` take a `scheduled` day using the same words as the
CLI, so an agent can plan your week:

`add_todo`, `list_todos`, `get_todo`, `complete_todo`, `update_todo`, `delete_todo`,
`list_projects`, `get_timer`.

`get_timer` is read-only: an agent can see what you're focused on, how long
is left or how far into overtime you are, but only you start and stop timers.

Register it with an MCP client. For Claude Code:

```sh
claude mcp add agentodo -s user \
  -e AGENTODO_STORE=$HOME/.local/share/agentodo/todos.json \
  -- $HOME/.cargo/bin/agentodo-mcp
```

Or for Claude Desktop, add to the `mcpServers` block:

```json
{
  "mcpServers": {
    "agentodo": {
      "command": "/home/you/.cargo/bin/agentodo-mcp",
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

Markdown / Obsidian export, due dates (deadlines, as opposed to the soft
scheduled day), and subtasks are intentionally out of scope.
The core store is structured so these slot in without reworking the CLI or MCP
layers — e.g. an `export` command would render the store to markdown while JSON
remains the source of truth.
