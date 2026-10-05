# agentodo

A small todo tracker you can drive from the command line or a vim-style TUI, and
expose to an agent over MCP. The todo logic lives in one core library; the CLI,
TUI and MCP server are thin adapters over it.

Originally created to have a Todo app/task manager over MCP, but has grown into a TUI with several of my wishlist features that I have started to use to manage my daily workload.

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

### Your data

Two files next to each other hold everything: `todos.json` (todos and their
recorded time) and `todos.log.jsonl` (the [change log](#change-log)). The log
is the full history, including deleted todos and the time recorded on them, so
never delete it. Neither file is backed up for you: to keep years of history
safe, point `$AGENTODO_STORE` at a synced or backed-up folder (the log follows
the store).

For scripting, `agentodo list --status all --json` prints every todo with its
`sessions`. Each session has `start` and `end` (UTC, whole seconds),
`paused_secs` (time worked is `end - start - paused_secs`), `planned_secs`
(the timer's block; absent for time added by hand), and the `project` and
`tags` the todo had when the session was recorded, so moving a todo later
doesn't re-file its past time.

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

agentodo add "write docs" --plan 2h    # budget time for it
agentodo edit 2 --plan none            # drop the budget

agentodo start 1             # focus timer on #1: what's left of its plan, else 25m
agentodo start 1 50m         # or 50, 1h, 1h30m
agentodo timer               # what's running: time left, or +01:12 over
agentodo pause               # interrupted? the clock stands still
agentodo resume              # and picks up where it left off
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
| `o` `O` | add below / above — `title !h +project #tag ~2h` |
| `i`, `cc` | edit / retype title |
| `dd`, `3dd` | delete |
| `x`, `+` `-`, `gp`, `e` | toggle done, priority, project, notes in `$EDITOR` |
| `u`, `C-r` | undo / redo (deleted todos come back with the same id) |
| `/` `n` `N` | search titles |
| `p`, `.`, `s` | project filter, show/hide done, toggle sort |
| `:q` `:project [name]` `:sort priority\|created` `:hide` `:show` | commands |
| `]c` `[c`, `:log`, `:seen` | jump between changed todos, change log, clear markers |
| `T`, `:stop` | start a focus timer on this todo (asks how long) / stop it |
| `P`, `:pause` `:resume` | pause / resume the timer |
| `S`, `gT` | stop the timer / jump to its todo, from anywhere |
| `t` | plan time for this todo (`45m`, `2h`); empty clears it |
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

- **Stopping records the time**, overtime included. `T`, `S` or `:stop` stops it,
  so does marking the todo done, and starting a timer on another todo
  switches. Deleting the todo drops it.
- **Pause for interruptions.** `P` (or `agentodo pause`) stops the clock
  without ending the session; `P` again resumes it. The pause isn't counted
  as work or toward overtime, and the session lists it (`10m paused`).
  Stopping while paused records the time up to the pause.
- **A forgotten timer pauses itself** an hour into overtime, as though you'd
  paused it then, so a timer left running over the weekend records 1h of
  overtime, not three days. It doesn't matter whether anything was open at
  the time. Resume it (`P`) if you really were still working: you get
  another hour from then. Set `$AGENTODO_TIMER_MAX_OVER` to change the limit
  (`2h`, `30m`) or to `off` to let timers run on until stopped. The limit is
  fixed when a timer starts.
- **Fix it afterwards.** If you walked away and the timer ran all night,
  correct the session instead of losing it. In the TUI, `Enter` on a todo
  lists its sessions newest first, with overtime called out. The newest
  session is selected; `c` takes a new length (`34m`), `dd` deletes, `a`
  adds time worked without the timer, and `u` undoes. From the shell:
  `agentodo time 3 34m`.
- **Undo never loses time.** `u` and `:log` revert restore everything about a
  todo except its sessions. Session edits have their own undo, and recorded
  sessions can't be reverted from `:log`.

While a timer runs, a player docks above the TUI's footer, like a music
player's now-playing bar: the todo, when the block ends, and elapsed time
against a progress bar. It's green in the block, yellow in overtime and red
once it's 15 minutes over and looks forgotten. On a short terminal it folds
into one line. The todo's row badges its id and counts down too (on the week
board, its card). Paused, it turns grey and shows `⏸`. When the block is up,
the TUI shows a banner and rings the terminal bell. Windows Terminal turns the bell into a taskbar flash. To
get a desktop notification as well, set `$AGENTODO_TIMER_HOOK` to a shell
command. It runs with `$AGENTODO_TODO_ID` and `$AGENTODO_TODO_TITLE` set.
For example, a Windows toast from WSL:

```sh
export AGENTODO_TIMER_HOOK='powershell.exe -NoProfile -Command "New-BurntToastNotification -Text \"Block done\", \"$AGENTODO_TODO_TITLE\""'
```

The bell only rings from an open TUI, and only if the block ended in the
last minute. A TUI opened later just shows the overtime. Set
`$AGENTODO_TIMER_MINUTES` to change the default block (e.g. `30`), and
`$AGENTODO_TIMER_MAX_OVER` the overtime limit (default `1h`, or `off`).

### Planning time

Give a todo a budget for the whole task, across however many sessions it
takes: `~2h` when adding or retitling, `t` on an existing one, `--plan 2h` in
the CLI, or `plan` over MCP. Then:

- The list shows time spent against the plan, `45m/2h`. It turns yellow once
  you're over.
- On the week board, each card shows what's left (`~1h15m`, or `~over`), and
  each day's header adds up what's left for its open todos (`Fri 25 3h30m`).
  An overloaded day is visible while you move cards around.
- `T` fills in what's left of the plan, falling back to the default block
  once it's used up.
- `Enter` shows `planned  2h · 45m spent · 1h15m left`.

A running timer counts toward all of these as it goes.

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

`add_todo` and `update_todo` also take a `plan` (`30m`, `2h`), so an agent can
budget your time as well as schedule it. `get_timer` is read-only: an agent can see what you're focused on, how long
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
