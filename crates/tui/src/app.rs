//! The TUI's state machine: keys in, store mutations and view state out.
//!
//! Nothing here touches the terminal, so every keymap is unit-testable. The
//! only effects that need the terminal (quitting, suspending for `$EDITOR`)
//! are returned to the caller as an [`Effect`].

use std::collections::HashSet;
use std::path::PathBuf;

use agentodo_core::{
    default_timer, format_duration, log_path, parse_duration, parse_quick_add, read_log,
    read_log_from, week_start, ActiveSession, EntryKind, Filter, LogEntry, NewTodo, Priority,
    ProjectSummary, Session, SortBy, Source, Status, Store, Todo, TodoPatch,
};
use chrono::{DateTime, Datelike, Days, Local, NaiveDate, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

use crate::line::LineEdit;
use crate::repo::Repo;

/// Something the event loop must do on the app's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    None,
    Quit,
    /// Open `$EDITOR` on these notes, then call [`App::finish_notes`].
    EditNotes {
        id: u64,
        notes: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Input {
        kind: InputKind,
        line: LineEdit,
    },
    Picker {
        items: Vec<ProjectFilter>,
        selected: usize,
    },
    /// `:log`. `entries` is `all` (newest first), narrowed when `others_only`.
    Log {
        all: Vec<LogEntry>,
        entries: Vec<LogEntry>,
        selected: usize,
        others_only: bool,
    },
    Help,
    /// `Enter`: one todo in full, with its sessions. `selected` counts from
    /// the newest session.
    Detail {
        id: u64,
        selected: usize,
    },
}

/// What a line of typed input is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    /// `o`/`O`: a new todo below/above the cursor.
    NewTodo { above: bool },
    /// `i`/`cc`: retitle an existing todo.
    EditTitle(u64),
    /// `gp`: set a todo's project.
    SetProject(u64),
    /// `:`
    Command,
    /// `/`, remembering where the cursor started so Esc can return there.
    Search { origin: usize },
    /// `T`: how long to focus on a todo.
    TimerLength(u64),
    /// `t`: the time budgeted for a todo; empty clears it.
    Plan(u64),
    /// `c` in the detail view: a new length for session `index` (oldest
    /// first) of todo `id`.
    SessionLength { id: u64, index: usize },
    /// `a` in the detail view: time worked without the timer.
    AddSession(u64),
}

/// Which screen normal mode is on. Overlays (help, log, picker…) work over
/// either.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    List,
    /// `Backlog | Mon … Sun`, planned by scheduled day.
    Week,
}

/// Columns on the week board: Backlog, then Monday to Sunday.
pub const WEEK_COLUMNS: usize = 8;

/// A todo as placed on the week board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub todo: Todo,
    /// Days an open todo has slipped past its scheduled day; 0 if it hasn't.
    pub slipped: i64,
}

/// Which project the list is narrowed to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProjectFilter {
    #[default]
    All,
    Unassigned,
    Named(String),
}

impl ProjectFilter {
    fn matches(&self, todo: &Todo) -> bool {
        let project = todo.project.as_deref().filter(|p| !p.is_empty());
        match self {
            ProjectFilter::All => true,
            ProjectFilter::Unassigned => project.is_none(),
            ProjectFilter::Named(name) => project == Some(name.as_str()),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            ProjectFilter::All => "all projects",
            ProjectFilter::Unassigned => "(no project)",
            ProjectFilter::Named(name) => name,
        }
    }
}

/// One reversible store edit. Undo/redo work on groups of these so `3dd` is a
/// single undo step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Created(Todo),
    Deleted(Todo),
    Updated {
        before: Todo,
        after: Todo,
    },
    /// Session edits, which snapshot restores leave alone.
    Sessions {
        id: u64,
        before: Vec<Session>,
        after: Vec<Session>,
    },
}

impl Change {
    fn id(&self) -> u64 {
        match self {
            Change::Created(t) | Change::Deleted(t) => t.id,
            Change::Updated { before, .. } => before.id,
            Change::Sessions { id, .. } => *id,
        }
    }

    fn revert(&self, store: &mut Store) -> agentodo_core::Result<()> {
        match self {
            Change::Created(t) => store.delete(t.id).map(drop),
            Change::Deleted(t) => store.restore(t.clone()).map(drop),
            Change::Updated { before, .. } => store.replace(before.clone()).map(drop),
            Change::Sessions { id, before, .. } => {
                store.set_sessions(*id, before.clone()).map(drop)
            }
        }
    }

    fn reapply(&self, store: &mut Store) -> agentodo_core::Result<()> {
        match self {
            Change::Created(t) => store.restore(t.clone()).map(drop),
            Change::Deleted(t) => store.delete(t.id).map(drop),
            Change::Updated { after, .. } => store.replace(after.clone()).map(drop),
            Change::Sessions { id, after, .. } => store.set_sessions(*id, after.clone()).map(drop),
        }
    }
}

/// The pending op after `gw`, waiting for a day key.
const DAY_PICK: char = '@';

/// Keys typed so far in normal mode that don't form a command yet (`3`, `d`, `g`…).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    pub count: Option<usize>,
    pub op: Option<char>,
}

impl Pending {
    /// Rendered like vim's `showcmd`.
    pub fn display(&self) -> String {
        let mut s = self.count.map(|c| c.to_string()).unwrap_or_default();
        match self.op {
            Some(DAY_PICK) => s.push_str("gw"),
            op => s.extend(op),
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub error: bool,
}

pub struct App {
    repo: Repo,
    /// Every todo in the store as of the last load.
    all: Vec<Todo>,
    projects: Vec<ProjectSummary>,
    /// The visible, filtered and sorted list.
    pub rows: Vec<Todo>,
    pub cursor: usize,
    /// The todo the cursor is on; the cursor follows it across reloads.
    cursor_id: Option<u64>,
    pub project: ProjectFilter,
    pub show_done: bool,
    pub sort: SortBy,
    pub mode: Mode,
    pub pending: Pending,
    pub search: Option<String>,
    pub message: Option<Message>,
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    /// Height of the list viewport, set by the renderer, for `Ctrl-d`/`Ctrl-u`.
    pub page: usize,
    pub table: TableState,
    log_path: PathBuf,
    /// How far into the change log we've read.
    log_offset: u64,
    /// Changes by others after this instant get a marker.
    seen: DateTime<Utc>,
    /// Todos changed by someone else since `seen` that you haven't moved past.
    pub changed: HashSet<u64>,
    pub view: View,
    /// The Monday of the week on the board.
    pub week: NaiveDate,
    /// The focused board column: 0 is Backlog, 1–7 are Monday–Sunday.
    pub col: usize,
    /// Today's date, refreshed on every reload so the board follows midnight.
    pub today: NaiveDate,
    clock: fn() -> NaiveDate,
    /// The running focus timer as of the last load.
    pub timer: Option<ActiveSession>,
    /// The timer whose end-of-block bell has gone, so it rings only once.
    rung: Option<ActiveSession>,
    now: fn() -> DateTime<Utc>,
}

/// A block that ended longer ago than this when the TUI first saw it (it
/// was closed, say) doesn't ring; the header already shows the overtime.
const STALE_ALERT: chrono::Duration = chrono::Duration::minutes(1);

fn local_today() -> NaiveDate {
    Local::now().date_naive()
}

impl App {
    /// `seen` is when the previous TUI session ended; changes by others after
    /// it are marked. `None` (first run) marks only changes from now on.
    pub fn new(repo: Repo, seen: Option<DateTime<Utc>>) -> anyhow::Result<Self> {
        let log_path = log_path(repo.path());
        let mut app = App {
            repo,
            all: Vec::new(),
            projects: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            cursor_id: None,
            project: ProjectFilter::All,
            show_done: true,
            sort: SortBy::Priority,
            mode: Mode::Normal,
            pending: Pending::default(),
            search: None,
            message: None,
            undo: Vec::new(),
            redo: Vec::new(),
            page: 20,
            table: TableState::default(),
            log_path,
            log_offset: 0,
            seen: seen.unwrap_or_else(Utc::now),
            changed: HashSet::new(),
            view: View::List,
            week: week_start(local_today()),
            col: 0,
            today: local_today(),
            clock: local_today,
            timer: None,
            rung: None,
            now: Utc::now,
        };
        let store = app.repo.load()?;
        app.absorb(store);
        Ok(app)
    }

    /// Every todo in the store as of the last load, unfiltered.
    pub fn all_todos(&self) -> &[Todo] {
        &self.all
    }

    /// The todo under the cursor, if any.
    pub fn current(&self) -> Option<&Todo> {
        self.rows.get(self.cursor)
    }

    /// `(open, done)` counts within the current project scope.
    pub fn counts(&self) -> (usize, usize) {
        let scoped = self.all.iter().filter(|t| self.project.matches(t));
        scoped.fold((0, 0), |(o, d), t| match t.status {
            Status::Open => (o + 1, d),
            Status::Done => (o, d + 1),
        })
    }

    /// Reloads if another process changed the store. Called periodically.
    pub fn refresh_if_changed(&mut self) {
        if self.repo.changed() {
            self.reload();
        }
    }

    pub fn reload(&mut self) {
        match self.repo.load() {
            Ok(store) => self.absorb(store),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn absorb(&mut self, store: Store) {
        self.today = (self.clock)();
        self.projects = store.projects();
        self.all = store.all().to_vec();
        self.timer = store.active_timer().copied();
        self.read_new_log_entries();
        self.rebuild_rows();
    }

    /// Marks todos touched by others in log entries we haven't read yet.
    fn read_new_log_entries(&mut self) {
        match read_log_from(&self.log_path, self.log_offset) {
            Ok((entries, offset)) => {
                self.log_offset = offset;
                let theirs = entries
                    .iter()
                    .filter(|e| e.actor.source != Source::Tui && e.at > self.seen);
                for entry in theirs {
                    match entry.kind() {
                        EntryKind::Deleted => self.changed.remove(&entry.id),
                        _ => self.changed.insert(entry.id),
                    };
                }
            }
            Err(e) => self.error(format!("reading change log: {e}")),
        }
    }

    fn rebuild_rows(&mut self) {
        if self.view == View::Week {
            self.rows = self.column(self.col).into_iter().map(|c| c.todo).collect();
            return self.follow_cursor();
        }
        let mut rows: Vec<Todo> = self
            .all
            .iter()
            .filter(|t| self.project.matches(t))
            .filter(|t| self.show_done || t.status == Status::Open)
            .cloned()
            .collect();
        Filter {
            sort: self.sort,
            ..Default::default()
        }
        .sort(&mut rows);
        // Stable, so open todos keep their sort order above done ones.
        rows.sort_by_key(|t| t.status == Status::Done);
        self.rows = rows;
        self.follow_cursor();
    }

    /// Follows the todo we were on; if it's gone, stays at the same height.
    fn follow_cursor(&mut self) {
        let found = self
            .cursor_id
            .and_then(|id| self.rows.iter().position(|t| t.id == id));
        self.select(found.unwrap_or(self.cursor));
    }

    fn select(&mut self, index: usize) {
        self.cursor = index.min(self.rows.len().saturating_sub(1));
        self.cursor_id = self.current().map(|t| t.id);
    }

    fn focus(&mut self, id: u64) {
        self.cursor_id = Some(id);
        if self.view == View::Week {
            self.place(id);
        }
        self.rebuild_rows();
    }

    // ---- focus timer ----------------------------------------------------------

    pub fn now(&self) -> DateTime<Utc> {
        (self.now)()
    }

    /// Pins the wall clock the timer runs on.
    #[cfg(test)]
    pub fn set_now(&mut self, now: fn() -> DateTime<Utc>) {
        self.now = now;
    }

    /// Notices the timer's planned block running out. Called every loop
    /// turn; returns the todo once per timer, when the caller should ring.
    /// The timer itself keeps running, counting overtime, until stopped.
    pub fn tick(&mut self) -> Option<Todo> {
        let now = self.now();
        let timer = self.timer.filter(|t| t.is_over(now))?;
        if self.rung == Some(timer) {
            return None;
        }
        self.rung = Some(timer);
        if now - timer.ends_at() >= STALE_ALERT {
            return None;
        }
        let todo = self.all.iter().find(|t| t.id == timer.todo)?.clone();
        self.info(format!(
            "⏱ {} up on #{} {} — counting overtime · T stops",
            format_duration(timer.ends_at() - timer.started_at),
            todo.id,
            todo.title
        ));
        Some(todo)
    }

    /// `T`: stop the timer if it's on this todo, else ask how long to start
    /// one for.
    fn timer_key(&mut self) {
        let Some(t) = self.current() else { return };
        let id = t.id;
        if self.timer.is_some_and(|a| a.todo == id) {
            return self.stop_timer();
        }
        if t.status == Status::Done {
            return self.error(format!("#{id} is done"));
        }
        // What's left of the plan, else the default block.
        let length = match t.plan_left(chrono::Duration::zero()) {
            Some(left) if left > chrono::Duration::zero() => format_duration(left),
            _ => default_timer().num_minutes().to_string(),
        };
        self.mode = Mode::Input {
            kind: InputKind::TimerLength(id),
            line: LineEdit::new(length),
        };
    }

    /// Time the running timer has put on `id` so far, if it's running there.
    pub fn running_on(&self, id: u64) -> chrono::Duration {
        self.timer
            .filter(|t| t.todo == id)
            .map_or(chrono::Duration::zero(), |t| {
                (self.now() - t.started_at).max(chrono::Duration::zero())
            })
    }

    /// What's left of `t`'s plan, counting a timer running on it.
    pub fn plan_left(&self, t: &Todo) -> Option<chrono::Duration> {
        t.plan_left(self.running_on(t.id))
    }

    fn set_plan(&mut self, id: u64, text: &str) {
        let plan = match text.trim() {
            "" => None,
            text => match parse_duration(text) {
                Some(plan) => Some(plan),
                None => {
                    return self.error(format!("not a duration: {text:?} (try 30m, 2h, 1h30m)"))
                }
            },
        };
        self.update(id, |s, id| {
            s.update(
                id,
                TodoPatch {
                    plan: Some(plan),
                    ..Default::default()
                },
            )
        });
    }

    fn start_timer(&mut self, id: u64, text: &str) {
        let Some(length) = parse_duration(text) else {
            return self.error(format!("not a duration: {text:?} (try 25, 50m, 1h30m)"));
        };
        let now = self.now();
        let result = self.repo.with_store(|s| {
            let stopped = s.stop_timer(now);
            s.start_timer(id, length, now)?;
            Ok(stopped)
        });
        self.reload();
        match result {
            Ok(stopped) => {
                let mut text = format!("⏱ {} on #{id}", format_duration(length));
                if let Some(prev) = stopped.filter(|p| p.id != id) {
                    let spent = prev
                        .sessions
                        .last()
                        .map(|s| s.duration())
                        .unwrap_or_default();
                    text.push_str(&format!(
                        " · stopped #{} after {}",
                        prev.id,
                        format_duration(spent)
                    ));
                }
                self.info(text);
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn stop_timer(&mut self) {
        let now = self.now();
        let result = self.repo.with_store(|s| Ok(s.stop_timer(now)));
        self.reload();
        match result {
            Ok(Some(todo)) => {
                let last = todo.sessions.last();
                let spent = last.map(|s| s.duration()).unwrap_or_default();
                let over = last
                    .and_then(Session::overtime)
                    .map(|o| format!(", {} over", format_duration(o)))
                    .unwrap_or_default();
                self.info(format!(
                    "stopped #{} after {}{over} ({} total) · Enter to fix",
                    todo.id,
                    format_duration(spent),
                    format_duration(todo.time_spent())
                ));
            }
            Ok(None) => self.info("no timer running"),
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    /// Applies a session edit to todo `id`, recording it for undo, and
    /// returns to its detail view with `selected` on the session shown.
    fn edit_sessions(
        &mut self,
        id: u64,
        selected: usize,
        f: impl FnOnce(&mut Store) -> agentodo_core::Result<Todo>,
    ) {
        let result = self.repo.with_store(|s| {
            let before = s
                .get(id)
                .map(|t| t.sessions.clone())
                .ok_or(agentodo_core::Error::NotFound(id))?;
            let after = f(s)?.sessions;
            Ok((before, after))
        });
        match result {
            Ok((before, after)) => self.record(vec![Change::Sessions { id, before, after }]),
            Err(e) => self.error(format!("{e:#}")),
        }
        self.reload();
        let count = self.todo(id).map_or(0, |t| t.sessions.len());
        self.mode = Mode::Detail {
            id,
            selected: selected.min(count.saturating_sub(1)),
        };
    }

    fn todo(&self, id: u64) -> Option<&Todo> {
        self.all.iter().find(|t| t.id == id)
    }

    /// Keys in the detail view: move between sessions and fix them. Any
    /// other key closes it, as it always has.
    fn detail_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Detail { id, selected } = self.mode else {
            return Effect::None;
        };
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = self.todo(id).map_or(0, |t| t.sessions.len());
        // Sessions are stored oldest first but listed newest first.
        let index = count.checked_sub(selected + 1);
        let pending_d = self.pending.op.take() == Some('d');
        let stay = |selected| Mode::Detail { id, selected };

        match key.code {
            KeyCode::Char('r') if ctrl => self.redo(),
            _ if ctrl => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                self.mode = stay((selected + 1).min(count.saturating_sub(1)))
            }
            KeyCode::Char('k') | KeyCode::Up => self.mode = stay(selected.saturating_sub(1)),
            KeyCode::Char('g') => self.mode = stay(0),
            KeyCode::Char('G') => self.mode = stay(count.saturating_sub(1)),
            KeyCode::Char('c') => match index {
                Some(index) => {
                    self.mode = Mode::Input {
                        kind: InputKind::SessionLength { id, index },
                        line: LineEdit::default(),
                    }
                }
                None => self.info("no time recorded yet — a adds some"),
            },
            KeyCode::Char('a') => {
                self.mode = Mode::Input {
                    kind: InputKind::AddSession(id),
                    line: LineEdit::default(),
                }
            }
            KeyCode::Char('d') if pending_d => {
                if let Some(index) = index {
                    self.edit_sessions(id, selected, |s| s.delete_session(id, index));
                }
            }
            KeyCode::Char('d') => self.pending.op = Some('d'),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('e') => {
                let notes = self.todo(id).and_then(|t| t.notes.clone());
                return Effect::EditNotes {
                    id,
                    notes: notes.unwrap_or_default(),
                };
            }
            _ => self.mode = Mode::Normal,
        }
        Effect::None
    }

    /// The detail view a session prompt returns to.
    fn back_to_detail(&mut self, kind: &InputKind) {
        let (id, index) = match *kind {
            InputKind::SessionLength { id, index } => (id, Some(index)),
            InputKind::AddSession(id) => (id, None),
            _ => return,
        };
        let count = self.todo(id).map_or(0, |t| t.sessions.len());
        let selected = index.map_or(0, |i| count.saturating_sub(i + 1));
        self.mode = Mode::Detail { id, selected };
    }

    fn commit_session(&mut self, kind: InputKind, text: &str) {
        self.back_to_detail(&kind);
        if text.trim().is_empty() {
            return;
        }
        let Some(length) = parse_duration(text) else {
            return self.error(format!("not a duration: {text:?} (try 34, 34m, 1h30m)"));
        };
        let Mode::Detail { selected, .. } = self.mode else {
            return;
        };
        let now = self.now();
        match kind {
            InputKind::SessionLength { id, index } => {
                self.edit_sessions(id, selected, |s| s.set_session_length(id, index, length))
            }
            InputKind::AddSession(id) => {
                self.edit_sessions(id, 0, |s| s.add_session(id, length, now))
            }
            _ => {}
        }
    }

    // ---- week board -----------------------------------------------------------

    /// Sets the view's clock; tests pin it to a known day.
    #[cfg(test)]
    pub fn set_clock(&mut self, clock: fn() -> NaiveDate) {
        self.clock = clock;
        self.today = clock();
        self.week = week_start(self.today);
        self.rebuild_rows();
    }

    /// The day board column `col` stands for, in the week on screen.
    pub fn column_day(&self, col: usize) -> Option<NaiveDate> {
        (col > 0).then(|| self.week + Days::new(col as u64 - 1))
    }

    fn day_column(day: NaiveDate) -> usize {
        day.weekday().num_days_from_monday() as usize + 1
    }

    /// The day a todo shows on: its scheduled day, except that open todos
    /// that slipped past it show on today. `None` means Backlog.
    fn shown_on(&self, t: &Todo) -> Option<NaiveDate> {
        match t.scheduled {
            Some(day) if t.status == Status::Open && day < self.today => Some(self.today),
            day => day,
        }
    }

    /// Which column a todo sits in on the week on screen, if any. Slipped
    /// todos move to today only on this week's board; past weeks keep them
    /// on the day they were planned for.
    fn column_of(&self, t: &Todo) -> Option<usize> {
        let day = match t.scheduled {
            None => return (t.status == Status::Open).then_some(0),
            Some(_) if week_start(self.today) == self.week => self.shown_on(t)?,
            Some(day) => day,
        };
        (week_start(day) == self.week).then(|| Self::day_column(day))
    }

    /// One board column's cards, sorted like the list with done ones last.
    pub fn column(&self, col: usize) -> Vec<Card> {
        let mut todos: Vec<Todo> = self
            .all
            .iter()
            .filter(|t| self.project.matches(t))
            .filter(|t| self.show_done || t.status == Status::Open)
            .filter(|t| self.column_of(t) == Some(col))
            .cloned()
            .collect();
        Filter {
            sort: self.sort,
            ..Default::default()
        }
        .sort(&mut todos);
        todos.sort_by_key(|t| t.status == Status::Done);
        todos
            .into_iter()
            .map(|todo| {
                let slipped = match todo.scheduled {
                    Some(day) if todo.status == Status::Open && day < self.today => {
                        (self.today - day).num_days()
                    }
                    _ => 0,
                };
                Card { todo, slipped }
            })
            .collect()
    }

    /// Moves the board to wherever todo `id` shows.
    fn place(&mut self, id: u64) {
        let Some(t) = self.all.iter().find(|t| t.id == id) else {
            return;
        };
        match (t.status, self.shown_on(t)) {
            (_, Some(day)) => {
                self.week = week_start(day);
                self.col = Self::day_column(day);
            }
            (Status::Open, None) => self.col = 0,
            (Status::Done, None) => {}
        }
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        if view == View::Week {
            self.week = week_start(self.today);
            self.col = Self::day_column(self.today);
            if let Some(id) = self.cursor_id {
                self.place(id);
            }
        }
        self.rebuild_rows();
    }

    fn move_column(&mut self, delta: isize) {
        let col = self.col as isize + delta;
        self.col = col.clamp(0, WEEK_COLUMNS as isize - 1) as usize;
        self.rebuild_rows();
    }

    fn move_week(&mut self, weeks: i64) {
        self.week = if weeks >= 0 {
            self.week + Days::new(7 * weeks as u64)
        } else {
            self.week - Days::new(7 * weeks.unsigned_abs())
        };
        self.rebuild_rows();
    }

    /// `L`/`H`: `days` later or earlier than the column the card is in.
    /// From Backlog, `L` lands on Monday; earlier than Monday is Backlog.
    fn shove(&mut self, days: i64) {
        let Some(id) = self.current().map(|t| t.id) else {
            return;
        };
        let day = match self.column_day(self.col) {
            None if days > 0 => Some(self.week + Days::new(days as u64 - 1)),
            None => return,
            Some(day) if days >= 0 => Some(day + Days::new(days as u64)),
            Some(day) => Some(day - Days::new(days.unsigned_abs())).filter(|d| *d >= self.week),
        };
        self.schedule(id, day);
    }

    /// Sets todo `id`'s scheduled day and follows it there.
    fn schedule(&mut self, id: u64, day: Option<NaiveDate>) {
        self.update(id, |s, id| {
            s.update(
                id,
                TodoPatch {
                    scheduled: Some(day),
                    ..Default::default()
                },
            )
        });
        self.focus(id);
    }

    fn schedule_current(&mut self, day: Option<NaiveDate>) {
        if let Some(id) = self.current().map(|t| t.id) {
            self.schedule(id, day);
        }
    }

    /// After `gw`: `m t w r f s u` pick a day of the week on screen, `b` Backlog.
    fn pick_day(&mut self, key: KeyCode) {
        let col = match key {
            KeyCode::Char('b') => 0,
            KeyCode::Char(c) => match "mtwrfsu".find(c) {
                Some(i) => i + 1,
                None => return,
            },
            _ => return,
        };
        // The list has no week on screen, so it picks from this week.
        let monday = match self.view {
            View::Week => self.week,
            View::List => week_start(self.today),
        };
        self.schedule_current((col > 0).then(|| monday + Days::new(col as u64 - 1)));
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: false,
        });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message {
            text: text.into(),
            error: true,
        });
    }

    // ---- key handling -------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> Effect {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        match self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Input { .. } => self.input_key(key),
            Mode::Picker { .. } => {
                self.picker_key(key);
                Effect::None
            }
            Mode::Log { .. } => {
                self.log_key(key);
                Effect::None
            }
            Mode::Detail { .. } => self.detail_key(key),
            Mode::Help => {
                self.mode = Mode::Normal;
                Effect::None
            }
        }
    }

    fn normal_key(&mut self, key: KeyEvent) -> Effect {
        let before = self.cursor_id;
        let effect = self.normal_key_inner(key);
        // Moving off a marked todo counts as having seen it.
        if let Some(id) = before.filter(|_| self.cursor_id != before) {
            self.changed.remove(&id);
        }
        effect
    }

    fn normal_key_inner(&mut self, key: KeyEvent) -> Effect {
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if let Some(op) = self.pending.op {
            let count = self.pending.count;
            self.pending = Pending::default();
            return match (op, key.code) {
                ('d', KeyCode::Char('d')) => {
                    self.delete_rows(count.unwrap_or(1));
                    Effect::None
                }
                ('g', KeyCode::Char('g')) => {
                    self.select(count.map_or(0, |n| n.saturating_sub(1)));
                    Effect::None
                }
                ('g', KeyCode::Char('b')) => {
                    self.schedule_current(None);
                    Effect::None
                }
                ('g', KeyCode::Char('t')) => {
                    self.schedule_current(Some(self.today));
                    Effect::None
                }
                ('g', KeyCode::Char('w')) => {
                    if self.current().is_some() {
                        self.pending.op = Some(DAY_PICK);
                    }
                    Effect::None
                }
                (DAY_PICK, key) => {
                    self.pick_day(key);
                    Effect::None
                }
                ('g', KeyCode::Char('p')) => {
                    self.start_input_on_current(|t| {
                        let project = t.project.clone().unwrap_or_default();
                        (InputKind::SetProject(t.id), LineEdit::new(project))
                    });
                    Effect::None
                }
                ('c', KeyCode::Char('c')) => {
                    self.start_input_on_current(|t| {
                        (InputKind::EditTitle(t.id), LineEdit::default())
                    });
                    Effect::None
                }
                ('Z', KeyCode::Char('Z')) => Effect::Quit,
                (']', KeyCode::Char('c')) => {
                    self.jump_to_change(true);
                    Effect::None
                }
                ('[', KeyCode::Char('c')) => {
                    self.jump_to_change(false);
                    Effect::None
                }
                _ => Effect::None,
            };
        }

        let count = self.pending.count;
        match key.code {
            KeyCode::Char(c @ '0'..='9') if !ctrl && (c != '0' || count.is_some()) => {
                let digit = c.to_digit(10).unwrap() as usize;
                self.pending.count =
                    Some(count.unwrap_or(0).saturating_mul(10).saturating_add(digit));
                return Effect::None;
            }
            KeyCode::Char(c @ ('d' | 'g' | 'c' | 'Z' | ']' | '[')) if !ctrl => {
                self.pending.op = Some(c);
                return Effect::None;
            }
            _ => {}
        }
        self.pending = Pending::default();
        let n = count.unwrap_or(1);

        match key.code {
            KeyCode::Char('d') if ctrl => self.select(self.cursor + (self.page / 2).max(1)),
            KeyCode::Char('u') if ctrl => {
                self.select(self.cursor.saturating_sub((self.page / 2).max(1)))
            }
            KeyCode::Char('r') if ctrl => self.redo(),
            KeyCode::Char('n') if ctrl && self.view == View::Week => self.move_week(n as i64),
            KeyCode::Char('p') if ctrl && self.view == View::Week => self.move_week(-(n as i64)),
            _ if ctrl => {}
            KeyCode::Char('W') => self.set_view(match self.view {
                View::List => View::Week,
                View::Week => View::List,
            }),
            KeyCode::Char('h') | KeyCode::Left if self.view == View::Week => {
                self.move_column(-(n as isize))
            }
            KeyCode::Char('l') | KeyCode::Right if self.view == View::Week => {
                self.move_column(n as isize)
            }
            KeyCode::Char('H') if self.view == View::Week => self.shove(-(n as i64)),
            KeyCode::Char('L') if self.view == View::Week => self.shove(n as i64),
            KeyCode::Char('j') | KeyCode::Down => self.select(self.cursor.saturating_add(n)),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.cursor.saturating_sub(n)),
            KeyCode::Char('G') => match count {
                Some(row) => self.select(row.saturating_sub(1)),
                None => self.select(usize::MAX),
            },
            KeyCode::Char('o') => self.start_new(false),
            KeyCode::Char('O') => self.start_new(true),
            KeyCode::Char('i') => self.start_input_on_current(|t| {
                (InputKind::EditTitle(t.id), LineEdit::new(t.title.clone()))
            }),
            KeyCode::Char('x') => self.toggle_done(),
            KeyCode::Char('T') => self.timer_key(),
            KeyCode::Char('t') => {
                self.start_input_on_current(|t| (InputKind::Plan(t.id), LineEdit::default()))
            }
            KeyCode::Char('+') | KeyCode::Char('=') => self.bump_priority(true),
            KeyCode::Char('-') => self.bump_priority(false),
            KeyCode::Char('e') => {
                if let Some(t) = self.current() {
                    return Effect::EditNotes {
                        id: t.id,
                        notes: t.notes.clone().unwrap_or_default(),
                    };
                }
            }
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('/') => {
                self.mode = Mode::Input {
                    kind: InputKind::Search {
                        origin: self.cursor,
                    },
                    line: LineEdit::default(),
                }
            }
            KeyCode::Char('n') => self.search_step(true),
            KeyCode::Char('N') => self.search_step(false),
            KeyCode::Char('p') => self.open_picker(),
            KeyCode::Char('.') => self.set_show_done(!self.show_done),
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    SortBy::Priority => SortBy::Created,
                    SortBy::Created => SortBy::Priority,
                };
                self.rebuild_rows();
            }
            KeyCode::Char(':') => {
                self.mode = Mode::Input {
                    kind: InputKind::Command,
                    line: LineEdit::default(),
                }
            }
            KeyCode::Char('q') => return Effect::Quit,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Enter => {
                if let Some(t) = self.current() {
                    self.mode = Mode::Detail {
                        id: t.id,
                        selected: 0,
                    };
                }
            }
            KeyCode::Esc => self.search = None,
            _ => {}
        }
        Effect::None
    }

    fn start_new(&mut self, above: bool) {
        self.mode = Mode::Input {
            kind: InputKind::NewTodo { above },
            line: LineEdit::default(),
        };
    }

    fn start_input_on_current(&mut self, make: impl FnOnce(&Todo) -> (InputKind, LineEdit)) {
        if let Some(t) = self.current() {
            let (kind, line) = make(t);
            self.mode = Mode::Input { kind, line };
        }
    }

    fn input_key(&mut self, key: KeyEvent) -> Effect {
        let Mode::Input { kind, line } = &mut self.mode else {
            return Effect::None;
        };
        let kind = kind.clone();

        match key.code {
            KeyCode::Enter => {
                let text = std::mem::take(&mut line.text);
                self.mode = Mode::Normal;
                return self.commit_input(kind, text);
            }
            KeyCode::Esc => {
                let text = std::mem::take(&mut line.text);
                self.mode = Mode::Normal;
                match kind {
                    // Like vim, leaving insert mode keeps what you typed.
                    InputKind::NewTodo { .. } | InputKind::EditTitle(_) => {
                        return self.commit_input(kind, text);
                    }
                    InputKind::Search { origin } => self.select(origin),
                    InputKind::SessionLength { .. } | InputKind::AddSession(_) => {
                        self.back_to_detail(&kind)
                    }
                    InputKind::Command
                    | InputKind::SetProject(_)
                    | InputKind::TimerLength(_)
                    | InputKind::Plan(_) => {}
                }
            }
            _ => {
                line.handle(key);
                if let InputKind::Search { origin } = kind {
                    // Incremental: preview the first match from where we started.
                    let term = line.text.clone();
                    match self.find(&term, origin, true, true) {
                        Some(i) => self.select(i),
                        None => self.select(origin),
                    }
                }
            }
        }
        Effect::None
    }

    fn commit_input(&mut self, kind: InputKind, text: String) -> Effect {
        match kind {
            InputKind::NewTodo { .. } => self.add(&text),
            InputKind::EditTitle(id) => self.retitle(id, &text),
            InputKind::SetProject(id) => {
                let project = Some(text.trim().to_owned()).filter(|p| !p.is_empty());
                self.update(id, |s, id| {
                    s.update(
                        id,
                        TodoPatch {
                            project: Some(project),
                            ..Default::default()
                        },
                    )
                });
            }
            InputKind::Command => return self.run_command(&text),
            InputKind::TimerLength(id) => self.start_timer(id, &text),
            InputKind::Plan(id) => self.set_plan(id, &text),
            InputKind::SessionLength { .. } | InputKind::AddSession(_) => {
                self.commit_session(kind, &text)
            }
            InputKind::Search { origin } => {
                if text.is_empty() {
                    self.select(origin);
                } else {
                    self.search = Some(text);
                    if let Some(i) = self
                        .search
                        .clone()
                        .and_then(|t| self.find(&t, origin, true, true))
                    {
                        self.select(i);
                    } else {
                        self.select(origin);
                        self.error(format!(
                            "Pattern not found: {}",
                            self.search.as_deref().unwrap_or("")
                        ));
                    }
                }
            }
        }
        Effect::None
    }

    // ---- change log -----------------------------------------------------------

    fn jump_to_change(&mut self, forward: bool) {
        let marked = |i: &usize| self.changed.contains(&self.rows[*i].id);
        let hit = if forward {
            (self.cursor + 1..self.rows.len()).find(marked)
        } else {
            (0..self.cursor).rev().find(marked)
        };
        match hit {
            Some(i) => self.select(i),
            None if self.changed.is_empty() => self.info("no changes by others"),
            None => self.info("no more changes in that direction"),
        }
    }

    fn open_log(&mut self) {
        match read_log(&self.log_path) {
            Ok(mut all) => {
                all.reverse();
                self.mode = Mode::Log {
                    entries: all.clone(),
                    all,
                    selected: 0,
                    others_only: false,
                };
            }
            Err(e) => self.error(format!("reading change log: {e}")),
        }
    }

    /// Re-reads the log after a revert, keeping the selection on `keep`.
    fn refresh_log(&mut self, keep: &LogEntry) {
        let Mode::Log { others_only, .. } = self.mode else {
            return;
        };
        let Ok(mut all) = read_log(&self.log_path) else {
            return;
        };
        all.reverse();
        let entries = log_filter(&all, others_only);
        let selected = entries.iter().position(|e| e == keep).unwrap_or(0);
        self.mode = Mode::Log {
            all,
            entries,
            selected,
            others_only,
        };
    }

    fn log_key(&mut self, key: KeyEvent) {
        self.message = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let half = (self.page / 2).max(1);
        let Mode::Log {
            all,
            entries,
            selected,
            others_only,
        } = &mut self.mode
        else {
            return;
        };
        let last = entries.len().saturating_sub(1);

        match key.code {
            KeyCode::Char('d') if ctrl => *selected = (*selected + half).min(last),
            KeyCode::Char('u') if ctrl => *selected = selected.saturating_sub(half),
            _ if ctrl => {}
            KeyCode::Char('j') | KeyCode::Down => *selected = (*selected + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Char('g') => *selected = 0,
            KeyCode::Char('G') => *selected = last,
            KeyCode::Char('a') => {
                *others_only = !*others_only;
                *entries = log_filter(all, *others_only);
                *selected = 0;
            }
            KeyCode::Enter => {
                if let Some(id) = entries.get(*selected).map(|e| e.id) {
                    self.mode = Mode::Normal;
                    self.goto_todo(id);
                }
            }
            KeyCode::Char('r') => {
                if let Some(entry) = entries.get(*selected).cloned() {
                    self.revert_entry(entry);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    /// Puts the cursor on `id`, widening the filters if they hide it.
    fn goto_todo(&mut self, id: u64) {
        self.reload();
        let Some(todo) = self.all.iter().find(|t| t.id == id).cloned() else {
            return self.error(format!("#{id} no longer exists"));
        };
        if !self.project.matches(&todo) {
            self.project = ProjectFilter::All;
        }
        if todo.status == Status::Done {
            self.show_done = true;
        }
        self.focus(id);
    }

    /// Undoes one logged change, but only if the todo is still exactly as that
    /// change left it; otherwise a newer change would be silently lost.
    fn revert_entry(&mut self, entry: LogEntry) {
        let id = entry.id;
        if entry.op == "session" {
            return self.error("recorded focus time can't be reverted");
        }
        let result = self.repo.with_store(|s| {
            let current = s.get(id).cloned();
            let change = match (entry.kind(), current) {
                (EntryKind::Created, Some(cur)) if unchanged_since(&cur, &entry) => {
                    Some(Change::Deleted(s.delete(id)?))
                }
                (EntryKind::Updated, Some(cur)) if unchanged_since(&cur, &entry) => {
                    let before = entry.before.clone().expect("updates have a before");
                    s.replace(before.clone())?;
                    Some(Change::Updated {
                        before: cur,
                        after: before,
                    })
                }
                (EntryKind::Deleted, None) => {
                    let todo = entry.before.clone().expect("deletes have a before");
                    Some(Change::Created(s.restore(todo)?))
                }
                _ => None,
            };
            Ok(change)
        });
        match result {
            Ok(Some(change)) => {
                self.record(vec![change]);
                self.info(format!("reverted #{id}"));
            }
            Ok(None) => self.error(format!("#{id} changed since — revert newer entries first")),
            Err(e) => self.error(format!("{e:#}")),
        }
        self.reload();
        self.refresh_log(&entry);
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Mode::Picker { items, selected } = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => *selected = (*selected + 1).min(items.len() - 1),
            KeyCode::Char('k') | KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Char('g') => *selected = 0,
            KeyCode::Char('G') => *selected = items.len() - 1,
            KeyCode::Enter => {
                let choice = items[*selected].clone();
                self.mode = Mode::Normal;
                self.set_project_filter(choice);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('p') => self.mode = Mode::Normal,
            _ => {}
        }
    }

    fn open_picker(&mut self) {
        let mut items = vec![ProjectFilter::All];
        items.extend(self.projects.iter().map(|p| match &p.project {
            Some(name) => ProjectFilter::Named(name.clone()),
            None => ProjectFilter::Unassigned,
        }));
        let selected = items.iter().position(|p| *p == self.project).unwrap_or(0);
        self.mode = Mode::Picker { items, selected };
    }

    /// Open/done counts for a picker entry.
    pub fn project_counts(&self, filter: &ProjectFilter) -> (usize, usize) {
        let summary = |name: Option<&str>| {
            self.projects
                .iter()
                .find(|p| p.project.as_deref() == name)
                .map_or((0, 0), |p| (p.open, p.done))
        };
        match filter {
            ProjectFilter::All => self
                .projects
                .iter()
                .fold((0, 0), |(o, d), p| (o + p.open, d + p.done)),
            ProjectFilter::Unassigned => summary(None),
            ProjectFilter::Named(name) => summary(Some(name)),
        }
    }

    fn set_show_done(&mut self, show: bool) {
        self.show_done = show;
        self.rebuild_rows();
        self.info(if show { "showing done" } else { "hiding done" });
    }

    fn set_project_filter(&mut self, filter: ProjectFilter) {
        self.project = filter;
        self.rebuild_rows();
    }

    fn run_command(&mut self, input: &str) -> Effect {
        let mut words = input.split_whitespace();
        let Some(cmd) = words.next() else {
            return Effect::None;
        };
        let arg = words.collect::<Vec<_>>().join(" ");

        match cmd {
            "q" | "q!" | "quit" | "wq" | "x" => return Effect::Quit,
            "w" => self.info("every change is saved as you make it"),
            "h" | "help" => self.mode = Mode::Help,
            "hide" => self.set_show_done(false),
            "log" => self.open_log(),
            "list" => self.set_view(View::List),
            "week" => {
                self.set_view(View::Week);
                self.week = week_start(self.today);
                self.col = Self::day_column(self.today);
                self.rebuild_rows();
            }
            "seen" => {
                self.changed.clear();
                self.info("change markers cleared");
            }
            "show" => self.set_show_done(true),
            "stop" => self.stop_timer(),
            "project" | "proj" => self.set_project_filter(if arg.is_empty() {
                ProjectFilter::All
            } else {
                ProjectFilter::Named(arg)
            }),
            "sort" => match arg.as_str() {
                "priority" | "p" => {
                    self.sort = SortBy::Priority;
                    self.rebuild_rows();
                }
                "created" | "c" => {
                    self.sort = SortBy::Created;
                    self.rebuild_rows();
                }
                _ => self.error("usage: :sort priority|created"),
            },
            n if n.parse::<usize>().is_ok() => {
                self.select(n.parse::<usize>().unwrap().saturating_sub(1))
            }
            other => self.error(format!("Not an editor command: {other}")),
        }
        Effect::None
    }

    // ---- search -------------------------------------------------------------

    /// Index of the next row whose title contains `term` (case-insensitive),
    /// scanning from `from` in the given direction and wrapping around.
    fn find(&self, term: &str, from: usize, forward: bool, inclusive: bool) -> Option<usize> {
        if term.is_empty() || self.rows.is_empty() {
            return None;
        }
        let term = term.to_lowercase();
        let len = self.rows.len();
        let start = if inclusive { 0 } else { 1 };
        (start..start + len)
            .map(|step| {
                if forward {
                    (from + step) % len
                } else {
                    (from + len * 2 - step) % len
                }
            })
            .find(|&i| self.rows[i].title.to_lowercase().contains(&term))
    }

    fn search_step(&mut self, forward: bool) {
        let Some(term) = self.search.clone() else {
            return self.error("No previous search");
        };
        match self.find(&term, self.cursor, forward, false) {
            Some(i) => self.select(i),
            None => self.error(format!("Pattern not found: {term}")),
        }
    }

    // ---- mutations ----------------------------------------------------------

    fn record(&mut self, changes: Vec<Change>) {
        if !changes.is_empty() {
            self.undo.push(changes);
            self.redo.clear();
        }
    }

    fn add(&mut self, text: &str) {
        let parsed = parse_quick_add(text);
        if parsed.title.is_empty() {
            if !text.trim().is_empty() {
                self.error("a todo needs a title");
            }
            return;
        }
        let project = parsed.project.or(match &self.project {
            ProjectFilter::Named(name) => Some(name.clone()),
            _ => None,
        });
        let new = NewTodo {
            title: parsed.title,
            priority: parsed.priority.unwrap_or_default(),
            project,
            tags: parsed.tags,
            notes: None,
            scheduled: match self.view {
                View::List => None,
                View::Week => self.column_day(self.col),
            },
            plan: parsed.plan,
        };
        match self.repo.with_store(|s| Ok(s.add(new))) {
            Ok(todo) => {
                let id = todo.id;
                self.record(vec![Change::Created(todo)]);
                self.reload();
                self.focus(id);
            }
            Err(e) => self.error(format!("{e:#}")),
        }
    }

    fn retitle(&mut self, id: u64, text: &str) {
        let parsed = parse_quick_add(text);
        if parsed.title.is_empty() {
            return;
        }
        let patch = TodoPatch {
            title: Some(parsed.title),
            priority: parsed.priority,
            project: parsed.project.map(Some),
            plan: parsed.plan.map(Some),
            add_tags: parsed.tags,
            ..Default::default()
        };
        self.update(id, |s, id| s.update(id, patch));
    }

    /// Applies `f` to one todo, recording a before/after snapshot for undo.
    fn update(&mut self, id: u64, f: impl FnOnce(&mut Store, u64) -> agentodo_core::Result<Todo>) {
        let result = self.repo.with_store(|s| {
            let before = s
                .get(id)
                .cloned()
                .ok_or(agentodo_core::Error::NotFound(id))?;
            let after = f(s, id)?;
            Ok((before, after))
        });
        match result {
            Ok((before, after)) if before != after => {
                self.record(vec![Change::Updated { before, after }])
            }
            Ok(_) => {}
            Err(e) => self.error(format!("{e:#}")),
        }
        self.reload();
    }

    fn toggle_done(&mut self) {
        let Some(t) = self.current() else { return };
        let status = match t.status {
            Status::Open => Status::Done,
            Status::Done => Status::Open,
        };
        let id = t.id;
        self.update(id, |s, id| s.set_status(id, status));
    }

    fn bump_priority(&mut self, up: bool) {
        let Some(t) = self.current() else { return };
        let priority = match (t.priority, up) {
            (Priority::Low, true) | (Priority::High, false) => Priority::Medium,
            (Priority::Medium, true) | (Priority::High, true) => Priority::High,
            (Priority::Medium, false) | (Priority::Low, false) => Priority::Low,
        };
        if priority == t.priority {
            return;
        }
        let id = t.id;
        self.update(id, |s, id| {
            s.update(
                id,
                TodoPatch {
                    priority: Some(priority),
                    ..Default::default()
                },
            )
        });
    }

    /// Stores notes edited in `$EDITOR`. An empty buffer clears them.
    pub fn finish_notes(&mut self, id: u64, notes: String) {
        let notes = Some(notes.trim_end().to_owned()).filter(|n| !n.is_empty());
        self.update(id, |s, id| {
            s.update(
                id,
                TodoPatch {
                    notes: Some(notes),
                    ..Default::default()
                },
            )
        });
    }

    fn delete_rows(&mut self, count: usize) {
        let ids: Vec<u64> = self
            .rows
            .iter()
            .skip(self.cursor)
            .take(count)
            .map(|t| t.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let result = self.repo.with_store(|s| {
            ids.iter()
                .map(|&id| s.delete(id).map(Change::Deleted))
                .collect::<agentodo_core::Result<Vec<_>>>()
        });
        match result {
            Ok(changes) => {
                let n = changes.len();
                self.record(changes);
                if n > 1 {
                    self.info(format!("{n} fewer todos"));
                }
            }
            Err(e) => self.error(format!("{e:#}")),
        }
        self.reload();
    }

    fn undo(&mut self) {
        let Some(group) = self.undo.pop() else {
            return self.info("Already at oldest change");
        };
        let result = self
            .repo
            .with_store(|s| group.iter().rev().try_for_each(|c| c.revert(s)));
        self.after_history(result, group, true);
    }

    fn redo(&mut self) {
        let Some(group) = self.redo.pop() else {
            return self.info("Already at newest change");
        };
        let result = self
            .repo
            .with_store(|s| group.iter().try_for_each(|c| c.reapply(s)));
        self.after_history(result, group, false);
    }

    fn after_history(&mut self, result: anyhow::Result<()>, group: Vec<Change>, undid: bool) {
        match result {
            Ok(()) => {
                let id = group[0].id();
                if undid {
                    self.redo.push(group);
                } else {
                    self.undo.push(group);
                }
                self.reload();
                self.focus(id);
            }
            Err(e) => {
                // The todo changed elsewhere in a way we can't reconcile; drop
                // the step rather than leaving the history stuck on it.
                self.error(format!(
                    "can't {}: {e:#}",
                    if undid { "undo" } else { "redo" }
                ));
                self.reload();
            }
        }
    }
}

fn log_filter(all: &[LogEntry], others_only: bool) -> Vec<LogEntry> {
    all.iter()
        .filter(|e| !others_only || e.actor.source != Source::Tui)
        .cloned()
        .collect()
}

/// True if `current` still matches the state `entry` left it in, ignoring
/// `updated_at` (an unlogged no-op edit can bump it) and sessions (time
/// tracked since is kept by any restore anyway).
fn unchanged_since(current: &Todo, entry: &LogEntry) -> bool {
    entry.after.as_ref().is_some_and(|after| {
        Todo {
            updated_at: current.updated_at,
            sessions: current.sessions.clone(),
            ..after.clone()
        } == *current
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A store file in a fresh temp dir, removed on drop.
    pub struct TempStore {
        pub dir: PathBuf,
    }

    impl TempStore {
        pub fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "agentodo-tui-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempStore { dir }
        }

        pub fn path(&self) -> PathBuf {
            self.dir.join("todos.json")
        }

        /// Seeds the store; titles may carry quick-add tokens.
        pub fn seed(&self, titles: &[&str]) {
            let mut store = Store::load(self.path()).unwrap();
            for title in titles {
                let q = parse_quick_add(title);
                store.add(NewTodo {
                    title: q.title,
                    priority: q.priority.unwrap_or_default(),
                    project: q.project,
                    tags: q.tags,
                    notes: None,
                    scheduled: None,
                    plan: q.plan,
                });
            }
            store.save().unwrap();
        }

        pub fn app(&self) -> App {
            App::new(Repo::new(self.path()), None).unwrap()
        }

        /// An app whose previous session ended at `seen`.
        pub fn app_seen(&self, seen: DateTime<Utc>) -> App {
            App::new(Repo::new(self.path()), Some(seen)).unwrap()
        }

        /// Edits the store the way an agent over MCP would.
        pub fn as_agent(&self, f: impl FnOnce(&mut Store)) {
            let mut store = self.store();
            store.set_actor(agentodo_core::Actor {
                source: Source::Mcp,
                client: Some("bot".into()),
            });
            f(&mut store);
            store.save().unwrap();
        }

        pub fn store(&self) -> Store {
            Store::load(self.path()).unwrap()
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    /// Feeds vim-notation keys: plain chars plus `<Esc>`, `<CR>`, `<BS>`,
    /// `<C-x>`.
    pub fn press(app: &mut App, keys: &str) -> Vec<Effect> {
        let mut effects = Vec::new();
        let mut rest = keys;
        while let Some(c) = rest.chars().next() {
            let (event, len) = if c == '<' {
                match rest.find('>') {
                    Some(end) if end > 1 => (special(&rest[1..end]), end + 1),
                    _ => (KeyEvent::new(KeyCode::Char('<'), KeyModifiers::NONE), 1),
                }
            } else {
                (
                    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                    c.len_utf8(),
                )
            };
            effects.push(app.handle_key(event));
            rest = &rest[len..];
        }
        effects
    }

    fn special(name: &str) -> KeyEvent {
        let none = KeyModifiers::NONE;
        match name {
            "Esc" => KeyEvent::new(KeyCode::Esc, none),
            "CR" => KeyEvent::new(KeyCode::Enter, none),
            "BS" => KeyEvent::new(KeyCode::Backspace, none),
            "Down" => KeyEvent::new(KeyCode::Down, none),
            ctrl if ctrl.starts_with("C-") => {
                let c = ctrl.chars().nth(2).unwrap();
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
            }
            other => panic!("unknown key <{other}>"),
        }
    }

    fn titles(app: &App) -> Vec<&str> {
        app.rows.iter().map(|t| t.title.as_str()).collect()
    }

    fn current_title(app: &App) -> &str {
        &app.current().unwrap().title
    }

    #[test]
    fn j_k_counts_and_gg_g() {
        let ts = TempStore::new();
        ts.seed(&["a", "b", "c", "d", "e"]);
        let mut app = ts.app();
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0);
        assert_eq!(titles(&app), ["e", "d", "c", "b", "a"]);

        press(&mut app, "j");
        assert_eq!(current_title(&app), "d");
        press(&mut app, "3j");
        assert_eq!(current_title(&app), "a");
        press(&mut app, "j"); // clamps at bottom
        assert_eq!(current_title(&app), "a");
        press(&mut app, "gg");
        assert_eq!(current_title(&app), "e");
        press(&mut app, "G");
        assert_eq!(current_title(&app), "a");
        press(&mut app, "2G");
        assert_eq!(current_title(&app), "d");
        press(&mut app, "4gg");
        assert_eq!(current_title(&app), "b");
        press(&mut app, "10k");
        assert_eq!(current_title(&app), "e");
    }

    #[test]
    fn dd_deletes_and_persists_immediately() {
        let ts = TempStore::new();
        ts.seed(&["a", "b", "c"]);
        let mut app = ts.app();
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0);

        press(&mut app, "jdd");
        assert_eq!(titles(&app), ["c", "a"]);
        assert_eq!(current_title(&app), "a"); // stays at the same height
        assert_eq!(ts.store().all().len(), 2);
    }

    #[test]
    fn count_dd_is_one_undo_step_and_restores_ids() {
        let ts = TempStore::new();
        ts.seed(&["a", "b", "c", "d"]);
        let mut app = ts.app();
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0);
        let before: Vec<Todo> = ts.store().all().to_vec();

        press(&mut app, "3dd");
        assert_eq!(titles(&app), ["a"]);

        press(&mut app, "u");
        let mut after = ts.store().all().to_vec();
        after.sort_by_key(|t| t.id);
        assert_eq!(after, before, "same ids and timestamps come back");
        assert_eq!(current_title(&app), "d");

        press(&mut app, "<C-r>");
        assert_eq!(titles(&app), ["a"]);
        press(&mut app, "u");
        assert_eq!(ts.store().all().len(), 4);
    }

    #[test]
    fn dd_on_last_row_moves_cursor_up() {
        let ts = TempStore::new();
        ts.seed(&["a", "b"]);
        let mut app = ts.app();
        press(&mut app, "Gdd");
        assert_eq!(app.rows.len(), 1);
        assert_eq!(app.cursor, 0);
        press(&mut app, "dd");
        assert!(app.rows.is_empty());
        press(&mut app, "dd"); // no-op on an empty list
        assert!(app.message.is_none());
    }

    #[test]
    fn o_adds_with_tokens_and_inherits_project_filter() {
        let ts = TempStore::new();
        ts.seed(&["existing +work"]);
        let mut app = ts.app();

        press(&mut app, ":project work<CR>");
        press(&mut app, "oship it !h #urgent<CR>");
        let t = app.current().unwrap().clone();
        assert_eq!(t.title, "ship it");
        assert_eq!(t.priority, Priority::High);
        assert_eq!(t.project.as_deref(), Some("work"));
        assert_eq!(t.tags, ["urgent"]);

        // An explicit +project beats the filter; Esc commits like vim.
        press(&mut app, "Oelsewhere +home<Esc>");
        assert_eq!(
            ts.store().all().last().unwrap().project.as_deref(),
            Some("home")
        );
        assert_eq!(app.rows.len(), 2, "home todo is outside the work filter");
    }

    #[test]
    fn empty_insert_is_cancelled_and_undo_removes_added() {
        let ts = TempStore::new();
        let mut app = ts.app();
        press(&mut app, "o<Esc>");
        assert!(ts.store().all().is_empty());

        press(&mut app, "onew<CR>");
        assert_eq!(ts.store().all().len(), 1);
        press(&mut app, "u");
        assert!(ts.store().all().is_empty());
    }

    #[test]
    fn i_edits_title_prefilled_and_cc_replaces() {
        let ts = TempStore::new();
        ts.seed(&["milk"]);
        let mut app = ts.app();

        press(&mut app, "i oat<CR>");
        assert_eq!(current_title(&app), "milk oat");
        press(&mut app, "ccbread !l<CR>");
        assert_eq!(current_title(&app), "bread");
        assert_eq!(app.current().unwrap().priority, Priority::Low);
        press(&mut app, "u");
        assert_eq!(current_title(&app), "milk oat");
    }

    #[test]
    fn x_toggles_and_priority_bumps_clamp() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();

        press(&mut app, "x");
        assert_eq!(app.current().unwrap().status, Status::Done);
        press(&mut app, "x");
        assert_eq!(app.current().unwrap().status, Status::Open);

        press(&mut app, "+++");
        assert_eq!(app.current().unwrap().priority, Priority::High);
        press(&mut app, "---");
        assert_eq!(app.current().unwrap().priority, Priority::Low);
        press(&mut app, "u");
        assert_eq!(app.current().unwrap().priority, Priority::Medium);
    }

    #[test]
    fn gp_sets_and_clears_project() {
        let ts = TempStore::new();
        ts.seed(&["a +old"]);
        let mut app = ts.app();
        press(&mut app, "gp<C-u>new<CR>");
        assert_eq!(app.current().unwrap().project.as_deref(), Some("new"));
        press(&mut app, "gp<C-u><CR>");
        assert_eq!(app.current().unwrap().project, None);
    }

    #[test]
    fn done_sorts_last_and_dot_hides_it() {
        let ts = TempStore::new();
        ts.seed(&["low !l", "high !h"]);
        let mut app = ts.app();
        assert_eq!(titles(&app), ["high", "low"]);
        press(&mut app, "x"); // complete "high"
        assert_eq!(titles(&app), ["low", "high"]);
        assert_eq!(current_title(&app), "high", "cursor follows the todo");

        press(&mut app, ".");
        assert_eq!(titles(&app), ["low"]);
        press(&mut app, ".");
        assert_eq!(titles(&app), ["low", "high"]);
    }

    #[test]
    fn hide_and_show_commands_set_done_visibility() {
        let ts = TempStore::new();
        ts.seed(&["open", "finished"]);
        let mut app = ts.app();
        press(&mut app, "/fin<CR>x");

        press(&mut app, ":hide<CR>");
        assert_eq!(titles(&app), ["open"]);
        press(&mut app, ":hide<CR>"); // idempotent, unlike `.`
        assert_eq!(titles(&app), ["open"]);
        press(&mut app, ":show<CR>");
        assert_eq!(titles(&app), ["open", "finished"]);
    }

    fn marked_titles(app: &App) -> Vec<&str> {
        app.rows
            .iter()
            .filter(|t| app.changed.contains(&t.id))
            .map(|t| t.title.as_str())
            .collect()
    }

    #[test]
    fn marks_other_actors_changes_since_last_session() {
        let ts = TempStore::new();
        ts.seed(&["old !h", "touched", "quiet !l"]);
        let seen = Utc::now();
        ts.as_agent(|s| {
            s.set_status(2, Status::Done).unwrap();
        });
        let mut app = ts.app_seen(seen);
        assert_eq!(marked_titles(&app), ["touched"], "seed predates seen");

        // Our own edits never get a marker; live agent edits do.
        press(&mut app, "x");
        ts.as_agent(|s| {
            s.add(NewTodo {
                title: "new from agent".into(),
                ..Default::default()
            });
        });
        app.refresh_if_changed();
        let mut marked = marked_titles(&app);
        marked.sort();
        assert_eq!(marked, ["new from agent", "touched"]);
    }

    #[test]
    fn first_session_marks_only_live_changes() {
        let ts = TempStore::new();
        ts.as_agent(|s| {
            s.add(NewTodo::default());
        });
        let mut app = ts.app();
        assert!(app.changed.is_empty());
        ts.as_agent(|s| {
            s.add(NewTodo::default());
        });
        app.reload();
        assert_eq!(app.changed.len(), 1);
    }

    #[test]
    fn jump_between_changes_and_clear_on_leave() {
        let ts = TempStore::new();
        ts.seed(&["a", "b", "c", "d"]);
        let seen = Utc::now();
        ts.as_agent(|s| {
            for id in [1, 3] {
                s.update(
                    id,
                    TodoPatch {
                        title: Some(format!("agent {id}")),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
        });
        let mut app = ts.app_seen(seen);
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0);
        assert_eq!(titles(&app), ["d", "agent 3", "b", "agent 1"]);

        press(&mut app, "]c");
        assert_eq!(current_title(&app), "agent 3");
        press(&mut app, "]c");
        assert_eq!(current_title(&app), "agent 1");
        assert_eq!(marked_titles(&app), ["agent 1"], "left agent 3 behind");
        press(&mut app, "]c");
        assert!(app.message.is_some(), "nothing further down");
        press(&mut app, ":seen<CR>");
        assert!(app.changed.is_empty());
    }

    fn log_entries(app: &App) -> Vec<(String, u64)> {
        let Mode::Log { entries, .. } = &app.mode else {
            panic!("not in log mode")
        };
        entries.iter().map(|e| (e.op.clone(), e.id)).collect()
    }

    #[test]
    fn log_lists_newest_first_and_filters_to_others() {
        let ts = TempStore::new();
        let mut app = ts.app();
        press(&mut app, "omine<CR>");
        ts.as_agent(|s| {
            s.add(NewTodo {
                title: "theirs".into(),
                ..Default::default()
            });
        });

        press(&mut app, ":log<CR>");
        assert_eq!(log_entries(&app), [("add".into(), 2), ("add".into(), 1)]);
        press(&mut app, "a");
        assert_eq!(log_entries(&app), [("add".into(), 2)]);
        press(&mut app, "q");
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn log_enter_jumps_to_todo_widening_filters() {
        let ts = TempStore::new();
        ts.seed(&["here +a", "there +b"]);
        let mut app = ts.app();
        press(&mut app, ":project a<CR>");
        press(&mut app, ":hide<CR>");
        ts.as_agent(|s| {
            s.set_status(2, Status::Done).unwrap();
        });

        press(&mut app, ":log<CR><CR>");
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(current_title(&app), "there");
        assert_eq!(app.project, ProjectFilter::All);
        assert!(app.show_done);
    }

    #[test]
    fn revert_restores_an_agent_delete_with_its_id_and_is_undoable() {
        let ts = TempStore::new();
        ts.seed(&["keep me"]);
        let original = ts.store().get(1).unwrap().clone();
        ts.as_agent(|s| {
            s.delete(1).unwrap();
        });
        let mut app = ts.app();

        press(&mut app, ":log<CR>r");
        assert_eq!(ts.store().get(1), Some(&original));
        assert!(matches!(app.mode, Mode::Log { .. }), "stays in the log");
        // The revert itself is logged (as ours) and selection stays put.
        assert_eq!(log_entries(&app)[0], ("restore".into(), 1));
        let Mode::Log { selected, .. } = app.mode else {
            unreachable!()
        };
        assert_eq!(selected, 1);

        press(&mut app, "q");
        press(&mut app, "u");
        assert!(ts.store().get(1).is_none());
    }

    #[test]
    fn revert_undoes_updates_and_creates() {
        let ts = TempStore::new();
        ts.seed(&["title"]);
        ts.as_agent(|s| {
            s.update(
                1,
                TodoPatch {
                    title: Some("agent title".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            s.add(NewTodo {
                title: "spam".into(),
                ..Default::default()
            });
        });
        let mut app = ts.app();

        press(&mut app, ":log<CR>r"); // newest: the add
        assert!(ts.store().get(2).is_none());
        // Selection stayed on the add (our revert was logged above it), so
        // the update is next.
        press(&mut app, "jr");
        assert_eq!(ts.store().get(1).unwrap().title, "title");
    }

    #[test]
    fn revert_refuses_when_todo_changed_since() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        ts.as_agent(|s| {
            s.set_status(1, Status::Done).unwrap();
            s.update(
                1,
                TodoPatch {
                    title: Some("later".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        });
        let mut app = ts.app();

        press(&mut app, ":log<CR>jr"); // the older set_status entry
        assert!(app.message.as_ref().unwrap().error);
        let t = ts.store().get(1).unwrap().clone();
        assert_eq!((t.title.as_str(), t.status), ("later", Status::Done));
    }

    #[test]
    fn cursor_follows_id_across_external_writes() {
        let ts = TempStore::new();
        ts.seed(&["a !l", "b !l"]);
        let mut app = ts.app();
        press(&mut app, "j");
        let id = app.current().unwrap().id;

        // An agent adds a higher-priority todo, which sorts above ours.
        let mut store = ts.store();
        store.add(NewTodo {
            title: "from agent".into(),
            priority: Priority::High,
            ..Default::default()
        });
        store.save().unwrap();

        app.reload();
        assert_eq!(app.rows[0].title, "from agent");
        assert_eq!(app.current().unwrap().id, id);
    }

    #[test]
    fn edits_do_not_clobber_external_writes() {
        let ts = TempStore::new();
        ts.seed(&["a", "b"]);
        let mut app = ts.app();

        let mut store = ts.store();
        store.add(NewTodo {
            title: "agent".into(),
            ..Default::default()
        });
        store.save().unwrap();

        press(&mut app, "dd"); // app has not reloaded yet
        let left: Vec<String> = ts.store().all().iter().map(|t| t.title.clone()).collect();
        assert!(left.contains(&"agent".to_string()));
        assert_eq!(left.len(), 2);
    }

    #[test]
    fn search_jumps_wraps_and_esc_restores() {
        let ts = TempStore::new();
        ts.seed(&["buy milk", "call mom", "Milkshake"]);
        let mut app = ts.app();
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0);
        assert_eq!(titles(&app), ["Milkshake", "call mom", "buy milk"]);

        press(&mut app, "j/milk<CR>");
        assert_eq!(current_title(&app), "buy milk");
        press(&mut app, "n");
        assert_eq!(current_title(&app), "Milkshake", "wraps, case-insensitive");
        press(&mut app, "N");
        assert_eq!(current_title(&app), "buy milk");

        press(&mut app, "gg/mom<Esc>");
        assert_eq!(current_title(&app), "Milkshake");
        press(&mut app, "/zzz<CR>");
        assert!(app.message.as_ref().unwrap().error);
    }

    #[test]
    fn picker_filters_by_project_including_unassigned() {
        let ts = TempStore::new();
        ts.seed(&["a +home", "b +work", "c"]);
        let mut app = ts.app();

        press(&mut app, "p");
        let Mode::Picker { items, .. } = &app.mode else {
            panic!()
        };
        assert_eq!(
            items,
            &[
                ProjectFilter::All,
                ProjectFilter::Named("home".into()),
                ProjectFilter::Named("work".into()),
                ProjectFilter::Unassigned,
            ]
        );
        press(&mut app, "jj<CR>");
        assert_eq!(titles(&app), ["b"]);
        press(&mut app, "pG<CR>");
        assert_eq!(titles(&app), ["c"]);
        press(&mut app, ":project<CR>");
        assert_eq!(app.rows.len(), 3);
    }

    #[test]
    fn commands_and_quit_keys() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();
        assert_eq!(press(&mut app, "q"), [Effect::Quit]);
        assert_eq!(press(&mut app, "ZZ")[1], Effect::Quit);
        assert_eq!(*press(&mut app, ":q<CR>").last().unwrap(), Effect::Quit);
        press(&mut app, ":sort created<CR>");
        assert_eq!(app.sort, SortBy::Created);
        press(&mut app, ":bogus<CR>");
        assert!(app.message.as_ref().unwrap().error);
        press(&mut app, "?");
        assert_eq!(app.mode, Mode::Help);
        press(&mut app, "<Esc>");
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn e_requests_editor_and_notes_are_undoable() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();
        let id = app.current().unwrap().id;
        assert_eq!(
            press(&mut app, "e"),
            [Effect::EditNotes {
                id,
                notes: String::new()
            }]
        );
        app.finish_notes(id, "line one\nline two\n".into());
        assert_eq!(
            app.current().unwrap().notes.as_deref(),
            Some("line one\nline two")
        );
        press(&mut app, "u");
        assert_eq!(app.current().unwrap().notes, None);
    }

    #[test]
    fn undo_of_externally_deleted_todo_reports_error() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();
        press(&mut app, "x");

        let mut store = ts.store();
        store.delete(1).unwrap();
        store.save().unwrap();

        press(&mut app, "u");
        assert!(app.message.as_ref().unwrap().error);
        assert!(app.rows.is_empty());
    }

    // ---- week board -----------------------------------------------------------

    fn ymd(m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap()
    }

    /// Friday 25 September 2026; the board's week is Mon 21 – Sun 27.
    fn friday() -> NaiveDate {
        ymd(9, 25)
    }

    /// Seeds `(title, scheduled)` todos in order, so ids start at 1.
    fn seed_days(ts: &TempStore, todos: &[(&str, Option<NaiveDate>)]) {
        let mut store = ts.store();
        for (title, scheduled) in todos {
            store.add(NewTodo {
                title: (*title).into(),
                scheduled: *scheduled,
                ..Default::default()
            });
        }
        store.save().unwrap();
    }

    fn week_app(ts: &TempStore) -> App {
        let mut app = ts.app();
        app.set_clock(friday);
        press(&mut app, ":week<CR>");
        app
    }

    fn column_titles(app: &App, col: usize) -> Vec<String> {
        app.column(col).into_iter().map(|c| c.todo.title).collect()
    }

    fn scheduled(ts: &TempStore, id: u64) -> Option<NaiveDate> {
        ts.store().get(id).unwrap().scheduled
    }

    #[test]
    fn board_places_todos_by_day_with_backlog_first() {
        let ts = TempStore::new();
        seed_days(
            &ts,
            &[
                ("loose", None),
                ("mon", Some(ymd(9, 21))),
                ("sun", Some(ymd(9, 27))),
                ("next week", Some(ymd(9, 28))),
            ],
        );
        let app = week_app(&ts);
        assert_eq!(app.week, ymd(9, 21));
        assert_eq!(app.col, 5, "opens on today");
        assert_eq!(column_titles(&app, 0), ["loose"]);
        assert_eq!(column_titles(&app, 1), Vec::<String>::new(), "mon slipped");
        assert_eq!(column_titles(&app, 7), ["sun"]);
        assert!(app.column(5).iter().any(|c| c.todo.title == "mon"));
    }

    #[test]
    fn slipped_todos_show_on_today_but_keep_their_date() {
        let ts = TempStore::new();
        seed_days(
            &ts,
            &[
                ("tuesday", Some(ymd(9, 22))),
                ("last week", Some(ymd(9, 16))),
            ],
        );
        let mut app = week_app(&ts);
        let today: Vec<(String, i64)> = app
            .column(5)
            .into_iter()
            .map(|c| (c.todo.title, c.slipped))
            .collect();
        assert!(today.contains(&("tuesday".into(), 3)));
        assert!(today.contains(&("last week".into(), 9)));
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 22)), "nothing rewritten");

        // Last week's board shows it where it was planned.
        press(&mut app, "<C-p>");
        assert_eq!(app.week, ymd(9, 14));
        assert_eq!(column_titles(&app, 3), ["last week"]);
        press(&mut app, "<C-n><C-n>");
        assert_eq!(app.week, ymd(9, 28));
        assert!((0..WEEK_COLUMNS).skip(1).all(|c| app.column(c).is_empty()));
    }

    #[test]
    fn done_todos_stay_on_their_day_and_leave_backlog() {
        let ts = TempStore::new();
        seed_days(&ts, &[("planned", Some(ymd(9, 22))), ("loose", None)]);
        let mut app = week_app(&ts);
        press(&mut app, "x"); // on today's column: the slipped "planned"
        assert_eq!(column_titles(&app, 2), ["planned"], "back on Tuesday");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 22)));

        press(&mut app, "hhhhhx");
        assert_eq!(app.col, 0);
        assert!(app.column(0).is_empty(), "done never sits in Backlog");

        press(&mut app, ":hide<CR>");
        assert!(app.column(2).is_empty());
    }

    #[test]
    fn shift_l_h_shove_a_day_and_the_cursor_follows() {
        let ts = TempStore::new();
        seed_days(&ts, &[("card", Some(ymd(9, 25)))]);
        let mut app = week_app(&ts);

        press(&mut app, "L");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 26)));
        assert_eq!((app.col, current_title(&app)), (6, "card"));
        press(&mut app, "H");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 25)));
        assert_eq!(app.col, 5);

        // Into the past it slips, so it stays on today.
        press(&mut app, "2H");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 23)));
        assert_eq!(app.col, 5);

        // Past Sunday rolls into next week and the board follows.
        press(&mut app, "3L");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 28)));
        assert_eq!((app.week, app.col), (ymd(9, 28), 1));

        // Earlier than Monday is Backlog; L from Backlog is Monday.
        press(&mut app, "H");
        assert_eq!(scheduled(&ts, 1), None);
        assert_eq!(app.col, 0);
        press(&mut app, "L");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 28)));
        assert_eq!(app.col, 1);
    }

    #[test]
    fn slipped_card_shoves_from_today() {
        let ts = TempStore::new();
        seed_days(&ts, &[("late", Some(ymd(9, 22)))]);
        let mut app = week_app(&ts);
        press(&mut app, "L");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 26)));
    }

    #[test]
    fn gb_gt_gw_send_a_card_somewhere() {
        let ts = TempStore::new();
        seed_days(&ts, &[("card", Some(ymd(9, 27)))]);
        let mut app = week_app(&ts);
        press(&mut app, "llL"); // over to Sunday, then shove into next Monday
        assert_eq!(app.week, ymd(9, 28));

        press(&mut app, "gt");
        assert_eq!(scheduled(&ts, 1), Some(friday()));
        assert_eq!((app.week, app.col), (ymd(9, 21), 5));

        press(&mut app, "gw");
        assert_eq!(app.pending.display(), "gw");
        press(&mut app, "r");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 24)));
        press(&mut app, "gwu");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 27)));
        press(&mut app, "gwb");
        assert_eq!(scheduled(&ts, 1), None);
        press(&mut app, "gwz"); // not a day: nothing happens
        assert_eq!(scheduled(&ts, 1), None);

        press(&mut app, "gb");
        assert_eq!(app.col, 0);
    }

    #[test]
    fn moves_undo_in_one_step() {
        let ts = TempStore::new();
        seed_days(&ts, &[("card", Some(ymd(9, 25)))]);
        let mut app = week_app(&ts);
        press(&mut app, "3L");
        press(&mut app, "u");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 25)));
        assert_eq!((app.week, app.col), (ymd(9, 21), 5), "follows it back");
        press(&mut app, "<C-r>");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 28)));
    }

    #[test]
    fn o_adds_to_the_focused_column() {
        let ts = TempStore::new();
        let mut app = week_app(&ts);
        press(&mut app, "lotomorrow<CR>");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 26)));
        press(&mut app, "6hosomeday !h<CR>");
        assert_eq!(scheduled(&ts, 2), None);
        assert_eq!(column_titles(&app, 0), ["someday"]);
    }

    #[test]
    fn h_l_move_between_columns_and_clamp() {
        let ts = TempStore::new();
        seed_days(&ts, &[("a", None), ("b", None), ("sat", Some(ymd(9, 26)))]);
        let mut app = week_app(&ts);
        press(&mut app, "5h");
        assert_eq!(app.col, 0);
        press(&mut app, "jh");
        assert_eq!((app.col, app.cursor), (0, 1));
        press(&mut app, "6l");
        assert_eq!((app.col, current_title(&app)), (6, "sat"), "row clamps");
        press(&mut app, "9l");
        assert_eq!(app.col, 7);
    }

    #[test]
    fn w_toggles_views_keeping_the_todo() {
        let ts = TempStore::new();
        seed_days(
            &ts,
            &[("thu", Some(ymd(9, 24))), ("far", Some(ymd(10, 15)))],
        );
        let mut app = ts.app();
        app.set_clock(friday);
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0);
        assert_eq!(current_title(&app), "far");

        press(&mut app, "W");
        assert_eq!(app.view, View::Week);
        assert_eq!((app.week, app.col), (ymd(10, 12), 4));
        assert_eq!(current_title(&app), "far");

        press(&mut app, "W");
        assert_eq!(app.view, View::List);
        assert_eq!(current_title(&app), "far");
        assert_eq!(app.rows.len(), 2);

        press(&mut app, ":week<CR>");
        assert_eq!((app.week, app.col), (ymd(9, 21), 5), ":week opens on today");
        press(&mut app, ":list<CR>");
        assert_eq!(app.view, View::List);
    }

    #[test]
    fn list_view_gt_schedules_without_leaving_the_list() {
        let ts = TempStore::new();
        seed_days(&ts, &[("card", None)]);
        let mut app = ts.app();
        app.set_clock(friday);
        press(&mut app, "gt");
        assert_eq!(scheduled(&ts, 1), Some(friday()));
        press(&mut app, "gwm");
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 21)));
        assert_eq!(app.view, View::List);
        press(&mut app, "L"); // week-only key
        assert_eq!(scheduled(&ts, 1), Some(ymd(9, 21)));
    }

    // ---- focus timer ------------------------------------------------------------

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-05T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn t10() -> DateTime<Utc> {
        t0() + chrono::Duration::minutes(10)
    }

    fn t25() -> DateTime<Utc> {
        t0() + chrono::Duration::minutes(25)
    }

    fn t90() -> DateTime<Utc> {
        t0() + chrono::Duration::minutes(90)
    }

    fn spent(ts: &TempStore, id: u64) -> i64 {
        ts.store().get(id).unwrap().time_spent().num_minutes()
    }

    #[test]
    fn t_prompts_for_minutes_starts_and_stops() {
        let ts = TempStore::new();
        ts.seed(&["focus"]);
        let mut app = ts.app();
        app.set_now(t0);

        press(&mut app, "T");
        let Mode::Input { kind, line } = &app.mode else {
            panic!("T should prompt")
        };
        assert_eq!(*kind, InputKind::TimerLength(1));
        assert_eq!(line.text, default_timer().num_minutes().to_string());

        press(&mut app, "<BS><BS><BS>50m<CR>");
        let timer = ts.store().active_timer().copied().unwrap();
        assert_eq!((timer.todo, timer.planned_secs), (1, 3000));
        assert_eq!(app.timer, Some(timer));

        app.set_now(t10);
        press(&mut app, "T");
        assert_eq!(app.timer, None);
        assert_eq!(spent(&ts, 1), 10);
        assert!(app.message.as_ref().unwrap().text.contains("after 10m"));
    }

    #[test]
    fn t_on_another_todo_switches_and_bad_input_is_refused() {
        let ts = TempStore::new();
        ts.seed(&["a", "b"]);
        let mut app = ts.app();
        app.sort = SortBy::Created;
        app.rebuild_rows();
        app.select(0); // b
        app.set_now(t0);
        press(&mut app, "T<CR>");
        assert_eq!(app.timer.unwrap().todo, 2);

        app.set_now(t10);
        press(&mut app, "jT<BS><BS>soon<CR>");
        assert!(app.message.as_ref().unwrap().error);
        assert_eq!(app.timer.unwrap().todo, 2, "a bad length keeps the timer");

        press(&mut app, "T<CR>");
        assert_eq!(app.timer.unwrap().todo, 1);
        assert_eq!(spent(&ts, 2), 10);
        assert!(app.message.as_ref().unwrap().text.contains("stopped #2"));

        press(&mut app, ":stop<CR>");
        assert_eq!(app.timer, None);
    }

    #[test]
    fn tick_rings_once_when_the_block_is_up_and_the_timer_runs_on() {
        let ts = TempStore::new();
        ts.seed(&["focus"]);
        let mut app = ts.app();
        app.set_now(t0);
        press(&mut app, "T<CR>");

        app.set_now(t10);
        assert_eq!(app.tick(), None, "still in the block");
        app.set_now(t25);
        assert_eq!(app.tick().unwrap().id, 1, "rings when the block is up");
        assert!(app
            .message
            .as_ref()
            .unwrap()
            .text
            .contains("counting overtime"));
        assert_eq!(app.tick(), None, "only once");
        assert!(ts.store().active_timer().is_some(), "and keeps running");
        assert_eq!(spent(&ts, 1), 0);

        // Opened long after the block ended: no bell, the header says it.
        let mut late = ts.app();
        late.set_now(t90);
        assert_eq!(late.tick(), None);

        app.set_now(t90);
        press(&mut app, "T");
        assert_eq!(spent(&ts, 1), 90, "overtime is recorded");
        assert!(app.message.as_ref().unwrap().text.contains("1h5m over"));
    }

    #[test]
    fn done_records_the_session_and_undo_keeps_it() {
        let ts = TempStore::new();
        ts.seed(&["focus"]);
        let mut app = ts.app();
        app.set_now(|| Utc::now() - chrono::Duration::minutes(30));
        press(&mut app, "T<CR>");
        press(&mut app, "x");
        assert_eq!(app.timer, None);
        assert_eq!(spent(&ts, 1), 30);

        press(&mut app, "u");
        assert_eq!(ts.store().get(1).unwrap().status, Status::Open);
        assert_eq!(spent(&ts, 1), 30, "undo never throws time away");
        assert_eq!(app.timer, None, "nor restarts the timer");
    }

    fn session_minutes(ts: &TempStore, id: u64) -> Vec<i64> {
        ts.store()
            .get(id)
            .unwrap()
            .sessions
            .iter()
            .map(|s| s.duration().num_minutes())
            .collect()
    }

    #[test]
    fn detail_view_fixes_adds_and_deletes_sessions_with_undo() {
        let ts = TempStore::new();
        ts.seed(&["focus"]);
        ts.as_agent(|s| {
            s.start_timer(1, chrono::Duration::minutes(25), t0())
                .unwrap();
            s.stop_timer(t10());
            s.start_timer(1, chrono::Duration::minutes(25), t25())
                .unwrap();
            s.stop_timer(t25() + chrono::Duration::minutes(590));
        });
        let mut app = ts.app();

        // The newest session is selected; `c` takes just a length.
        press(&mut app, "<CR>c34m<CR>");
        assert_eq!(session_minutes(&ts, 1), [10, 34]);
        assert_eq!(app.mode, Mode::Detail { id: 1, selected: 0 });

        press(&mut app, "jc");
        assert_eq!(
            app.mode,
            Mode::Input {
                kind: InputKind::SessionLength { id: 1, index: 0 },
                line: LineEdit::default()
            }
        );
        press(&mut app, "<Esc>");
        assert_eq!(
            app.mode,
            Mode::Detail { id: 1, selected: 1 },
            "Esc goes back"
        );
        press(&mut app, "dd");
        assert_eq!(session_minutes(&ts, 1), [34]);

        press(&mut app, "a45<CR>");
        assert_eq!(session_minutes(&ts, 1), [34, 45]);
        press(&mut app, "c<CR>");
        assert_eq!(
            session_minutes(&ts, 1),
            [34, 45],
            "empty input changes nothing"
        );
        press(&mut app, "csoon<CR>");
        assert!(app.message.as_ref().unwrap().error);

        press(&mut app, "uu");
        assert_eq!(session_minutes(&ts, 1), [10, 34]);
        assert!(
            matches!(app.mode, Mode::Detail { id: 1, .. }),
            "undo stays put"
        );
        press(&mut app, "<C-r>");
        assert_eq!(session_minutes(&ts, 1), [34]);

        press(&mut app, "x");
        assert_eq!(app.mode, Mode::Normal, "other keys close it");
        assert_eq!(ts.store().get(1).unwrap().status, Status::Open);
    }

    #[test]
    fn detail_view_moves_within_the_sessions_and_opens_notes() {
        let ts = TempStore::new();
        ts.seed(&["focus"]);
        ts.as_agent(|s| {
            for _ in 0..3 {
                s.add_session(1, chrono::Duration::minutes(5), t0())
                    .unwrap();
            }
        });
        let mut app = ts.app();
        press(&mut app, "<CR>jjjj");
        assert_eq!(app.mode, Mode::Detail { id: 1, selected: 2 });
        press(&mut app, "k");
        assert_eq!(app.mode, Mode::Detail { id: 1, selected: 1 });
        let effects = press(&mut app, "e");
        assert!(matches!(effects[0], Effect::EditNotes { id: 1, .. }));
    }

    #[test]
    fn session_log_entries_cant_be_reverted_but_older_edits_still_can() {
        let ts = TempStore::new();
        ts.seed(&["focus"]);
        let mut app = ts.app();
        press(&mut app, "+");
        app.set_now(t0);
        press(&mut app, "T<CR>");
        app.set_now(t10);
        press(&mut app, "T");

        press(&mut app, ":log<CR>r");
        assert!(app.message.as_ref().unwrap().error);
        assert_eq!(spent(&ts, 1), 10);

        press(&mut app, "jr");
        assert_eq!(ts.store().get(1).unwrap().priority, Priority::Medium);
        assert_eq!(spent(&ts, 1), 10);
    }

    #[test]
    fn timers_started_elsewhere_show_up_and_done_todos_refuse_one() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();
        ts.as_agent(|s| {
            s.start_timer(1, chrono::Duration::minutes(5), t0())
                .unwrap();
        });
        app.refresh_if_changed();
        assert_eq!(app.timer.unwrap().todo, 1);

        ts.as_agent(|s| {
            s.stop_timer(t10());
            s.set_status(1, Status::Done).unwrap();
        });
        app.refresh_if_changed();
        press(&mut app, "T");
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.message.as_ref().unwrap().error);
    }

    // ---- plans ------------------------------------------------------------------

    fn plan_minutes(ts: &TempStore, id: u64) -> Option<i64> {
        ts.store().get(id).unwrap().plan().map(|p| p.num_minutes())
    }

    #[test]
    fn plans_come_from_the_tilde_token_and_t() {
        let ts = TempStore::new();
        ts.seed(&["existing"]);
        let mut app = ts.app();

        press(&mut app, "owrite docs ~1h30m +work<CR>");
        let id = app.current().unwrap().id;
        assert_eq!(app.current().unwrap().title, "write docs");
        assert_eq!(plan_minutes(&ts, id), Some(90));

        press(&mut app, "t45m<CR>");
        assert_eq!(plan_minutes(&ts, id), Some(45));
        press(&mut app, "tsoon<CR>");
        assert!(app.message.as_ref().unwrap().error);
        assert_eq!(plan_minutes(&ts, id), Some(45));

        press(&mut app, "t<CR>");
        assert_eq!(plan_minutes(&ts, id), None, "empty clears");
        press(&mut app, "u");
        assert_eq!(plan_minutes(&ts, id), Some(45));

        // Retitling with a token replans; without one the plan stays.
        press(&mut app, "ccwrite more docs ~2h<CR>");
        assert_eq!(plan_minutes(&ts, id), Some(120));
        press(&mut app, "ccwrite docs<CR>");
        assert_eq!(plan_minutes(&ts, id), Some(120));
    }

    #[test]
    fn t_fills_in_what_is_left_of_the_plan() {
        let ts = TempStore::new();
        ts.seed(&["refactor ~2h"]);
        ts.as_agent(|s| {
            s.add_session(1, chrono::Duration::minutes(45), t0())
                .unwrap();
        });
        let mut app = ts.app();
        press(&mut app, "T");
        let Mode::Input { line, .. } = &app.mode else {
            panic!("T should prompt")
        };
        assert_eq!(line.text, "1h15m");

        // Used up: back to the default block.
        press(&mut app, "<Esc>");
        ts.as_agent(|s| {
            s.add_session(1, chrono::Duration::hours(2), t0()).unwrap();
        });
        app.reload();
        press(&mut app, "T");
        let Mode::Input { line, .. } = &app.mode else {
            panic!("T should prompt")
        };
        assert_eq!(line.text, default_timer().num_minutes().to_string());
    }

    #[test]
    fn plan_left_counts_the_running_timer() {
        let ts = TempStore::new();
        ts.seed(&["refactor ~1h"]);
        let mut app = ts.app();
        app.set_now(t0);
        press(&mut app, "T<CR>");
        app.set_now(t25);
        let t = app.current().unwrap().clone();
        assert_eq!(app.plan_left(&t), Some(chrono::Duration::minutes(35)));
        app.set_now(t90);
        assert_eq!(app.plan_left(&t), Some(chrono::Duration::zero()));
    }
}
