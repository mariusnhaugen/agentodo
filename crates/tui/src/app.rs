//! The TUI's state machine: keys in, store mutations and view state out.
//!
//! Nothing here touches the terminal, so every keymap is unit-testable. The
//! only effects that need the terminal (quitting, suspending for `$EDITOR`)
//! are returned to the caller as an [`Effect`].

use agentodo_core::{
    parse_quick_add, Filter, NewTodo, Priority, ProjectSummary, SortBy, Status, Store, Todo,
    TodoPatch,
};
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
    Help,
    Detail,
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
    Updated { before: Todo, after: Todo },
}

impl Change {
    fn id(&self) -> u64 {
        match self {
            Change::Created(t) | Change::Deleted(t) => t.id,
            Change::Updated { before, .. } => before.id,
        }
    }

    fn revert(&self, store: &mut Store) -> agentodo_core::Result<()> {
        match self {
            Change::Created(t) => store.delete(t.id).map(drop),
            Change::Deleted(t) => store.restore(t.clone()).map(drop),
            Change::Updated { before, .. } => store.replace(before.clone()).map(drop),
        }
    }

    fn reapply(&self, store: &mut Store) -> agentodo_core::Result<()> {
        match self {
            Change::Created(t) => store.restore(t.clone()).map(drop),
            Change::Deleted(t) => store.delete(t.id).map(drop),
            Change::Updated { after, .. } => store.replace(after.clone()).map(drop),
        }
    }
}

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
        s.extend(self.op);
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
}

impl App {
    pub fn new(repo: Repo) -> anyhow::Result<Self> {
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
        };
        let store = app.repo.load()?;
        app.absorb(store);
        Ok(app)
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
        self.projects = store.projects();
        self.all = store.all().to_vec();
        self.rebuild_rows();
    }

    fn rebuild_rows(&mut self) {
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

        // Follow the todo we were on; if it's gone, stay at the same height.
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
        self.rebuild_rows();
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
            Mode::Help | Mode::Detail => {
                self.mode = Mode::Normal;
                Effect::None
            }
        }
    }

    fn normal_key(&mut self, key: KeyEvent) -> Effect {
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
            KeyCode::Char(c @ ('d' | 'g' | 'c' | 'Z')) if !ctrl => {
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
            _ if ctrl => {}
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
            KeyCode::Enter if self.current().is_some() => self.mode = Mode::Detail,
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
                    InputKind::Command | InputKind::SetProject(_) => {}
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
            "show" => self.set_show_done(true),
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
                });
            }
            store.save().unwrap();
        }

        pub fn app(&self) -> App {
            App::new(Repo::new(self.path())).unwrap()
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
}
