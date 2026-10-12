use std::path::PathBuf;

use agentodo_core::{
    default_timer, format_duration, format_timer, parse_day, parse_duration,
    parse_scheduled_filter, ActiveSession, Actor, Filter, NewTodo, Priority, ProjectSummary,
    SortBy, Source, Status, Store, Todo, TodoPatch,
};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, NaiveDate, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// A todo tracker you can drive from the command line.
#[derive(Debug, Parser)]
#[command(name = "agentodo", version, about)]
struct Cli {
    /// Path to the JSON store. Overrides $AGENTODO_STORE and the default location.
    #[arg(long, global = true, value_name = "PATH")]
    store: Option<PathBuf>,

    /// Emit machine-readable JSON instead of a table.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Add a new todo.
    Add(AddArgs),
    /// List todos, optionally filtered.
    List(ListArgs),
    /// Show a single todo in full.
    Show { id: u64 },
    /// Mark a todo as done.
    Done { id: u64 },
    /// Mark a done todo as open again.
    Undone { id: u64 },
    /// Edit fields of an existing todo.
    Edit(EditArgs),
    /// Remove a todo.
    Rm { id: u64 },
    /// List distinct projects with todo counts.
    Projects,
    /// Start a focus timer on a todo, ending any running one.
    Start {
        id: u64,
        /// How long: minutes (25) or units (50m, 1h30m). Defaults to what's
        /// left of the todo's plan, else $AGENTODO_TIMER_MINUTES, else 25.
        duration: Option<String>,
    },
    /// Stop the focus timer, recording the time so far (overtime included).
    Stop,
    /// Pause the focus timer; the pause isn't counted as work.
    Pause,
    /// Resume a paused focus timer.
    Resume,
    /// Show the running focus timer.
    Timer,
    /// List a todo's recorded sessions, or fix one.
    ///
    /// `agentodo time 3 34m` changes the newest session to 34 minutes;
    /// `--session 2` picks the one before it (numbers as listed).
    Time(TimeArgs),
}

#[derive(Debug, Args)]
struct TimeArgs {
    id: u64,
    /// New length for the session: minutes (34) or units (34m, 1h30m).
    length: Option<String>,
    /// Which session, as numbered in the listing; 1 is the newest.
    #[arg(short, long, default_value_t = 1)]
    session: usize,
    /// Record time worked without the timer, ending now.
    #[arg(long, value_name = "LENGTH", conflicts_with_all = ["length", "rm"])]
    add: Option<String>,
    /// Delete the session instead.
    #[arg(long, conflicts_with = "length")]
    rm: bool,
}

#[derive(Debug, Args)]
struct AddArgs {
    /// The todo's title.
    title: String,
    #[arg(short, long, value_enum, default_value_t = PriorityArg::Medium)]
    priority: PriorityArg,
    #[arg(long)]
    project: Option<String>,
    /// Tag to attach. Repeat for multiple tags.
    #[arg(short, long = "tag", value_name = "TAG")]
    tags: Vec<String>,
    #[arg(short, long)]
    notes: Option<String>,
    /// Day to work on it: today, tomorrow, mon…sun or YYYY-MM-DD.
    #[arg(short, long, value_name = "DAY")]
    scheduled: Option<String>,
    /// Time budgeted for the whole todo: 30m, 2h, 1h30m.
    #[arg(long, value_name = "LENGTH")]
    plan: Option<String>,
}

#[derive(Debug, Args)]
struct ListArgs {
    #[arg(long, value_enum, default_value_t = StatusFilter::Open)]
    status: StatusFilter,
    #[arg(short, long, value_enum)]
    priority: Option<PriorityArg>,
    #[arg(short, long)]
    tag: Option<String>,
    #[arg(long)]
    project: Option<String>,
    /// Only todos scheduled on a day (today, fri, YYYY-MM-DD…), this week, or none.
    #[arg(short, long, value_name = "DAY|week|none")]
    scheduled: Option<String>,
    #[arg(long, value_enum, default_value_t = SortArg::Priority)]
    sort: SortArg,
}

#[derive(Debug, Args)]
struct EditArgs {
    id: u64,
    #[arg(long)]
    title: Option<String>,
    #[arg(short, long, value_enum)]
    priority: Option<PriorityArg>,
    /// Set the project. Pass an empty string to clear it.
    #[arg(long)]
    project: Option<String>,
    /// Set the notes. Pass an empty string to clear them.
    #[arg(short, long)]
    notes: Option<String>,
    #[arg(long = "add-tag", value_name = "TAG")]
    add_tag: Vec<String>,
    #[arg(long = "remove-tag", value_name = "TAG")]
    remove_tag: Vec<String>,
    /// Set the day to work on it (today, fri, YYYY-MM-DD…), or `none` to unschedule.
    #[arg(short, long, value_name = "DAY|none")]
    scheduled: Option<String>,
    /// Set the time budgeted for it (30m, 2h…), or `none` to clear it.
    #[arg(long, value_name = "LENGTH|none")]
    plan: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PriorityArg {
    Low,
    Medium,
    High,
}

impl From<PriorityArg> for Priority {
    fn from(p: PriorityArg) -> Self {
        match p {
            PriorityArg::Low => Priority::Low,
            PriorityArg::Medium => Priority::Medium,
            PriorityArg::High => Priority::High,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum StatusFilter {
    Open,
    Done,
    All,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SortArg {
    Priority,
    Created,
}

impl From<SortArg> for SortBy {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Priority => SortBy::Priority,
            SortArg::Created => SortBy::Created,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = Store::resolve_path(cli.store.clone()).context("determining default store path")?;
    let mut store =
        Store::load(&path).with_context(|| format!("loading store at {}", path.display()))?;
    store.set_actor(Actor::from_env(Source::Cli));
    let now = Utc::now();

    match cli.command {
        Command::Add(args) => {
            let todo = store.add(NewTodo {
                title: args.title,
                priority: args.priority.into(),
                project: args.project,
                tags: args.tags,
                notes: args.notes,
                scheduled: args.scheduled.as_deref().map(day_arg).transpose()?,
                plan: args.plan.as_deref().map(duration_arg).transpose()?,
            });
            store.save()?;
            print_one(&todo, cli.json)?;
        }
        Command::List(args) => {
            let filter = Filter {
                status: match args.status {
                    StatusFilter::Open => Some(Status::Open),
                    StatusFilter::Done => Some(Status::Done),
                    StatusFilter::All => None,
                },
                priority: args.priority.map(Into::into),
                tag: args.tag,
                project: args.project,
                scheduled: args
                    .scheduled
                    .as_deref()
                    .map(|s| {
                        parse_scheduled_filter(s, today()).ok_or_else(|| {
                            anyhow!("unknown day {s:?}: try today, fri, week, none or YYYY-MM-DD")
                        })
                    })
                    .transpose()?,
                sort: args.sort.into(),
            };
            let todos = store.list(&filter);
            print_list(&todos, cli.json)?;
        }
        Command::Show { id } => {
            let todo = store
                .get(id)
                .with_context(|| format!("no todo with id {id}"))?;
            print_one(todo, cli.json)?;
            if !cli.json {
                if let Some(active) = store.active_timer().filter(|a| a.todo == id) {
                    println!("  timer:    {}", timer_state(active, now));
                }
                if !todo.sessions.is_empty() {
                    println!("  sessions:");
                    print_sessions(todo, 10);
                }
            }
        }
        Command::Done { id } => {
            let todo = store.set_status(id, Status::Done)?;
            store.save()?;
            print_one(&todo, cli.json)?;
        }
        Command::Undone { id } => {
            let todo = store.set_status(id, Status::Open)?;
            store.save()?;
            print_one(&todo, cli.json)?;
        }
        Command::Edit(args) => {
            let id = args.id;
            let patch = TodoPatch {
                title: args.title,
                priority: args.priority.map(Into::into),
                project: args
                    .project
                    .map(|p| if p.is_empty() { None } else { Some(p) }),
                notes: args
                    .notes
                    .map(|n| if n.is_empty() { None } else { Some(n) }),
                add_tags: args.add_tag,
                remove_tags: args.remove_tag,
                scheduled: match args.scheduled.as_deref() {
                    None => None,
                    Some("none") => Some(None),
                    Some(s) => Some(Some(day_arg(s)?)),
                },
                plan: match args.plan.as_deref() {
                    None => None,
                    Some("none" | "") => Some(None),
                    Some(s) => Some(Some(duration_arg(s)?)),
                },
            };
            let todo = store.update(id, patch)?;
            store.save()?;
            print_one(&todo, cli.json)?;
        }
        Command::Rm { id } => {
            let todo = store.delete(id)?;
            store.save()?;
            if cli.json {
                print_one(&todo, true)?;
            } else {
                println!("removed #{} {}", todo.id, todo.title);
            }
        }
        Command::Projects => {
            print_projects(&store.projects(), cli.json)?;
        }
        Command::Start { id, duration } => {
            let length = match duration.as_deref() {
                None => store
                    .get(id)
                    .and_then(|t| t.plan_left(chrono::Duration::zero()))
                    .filter(|left| *left > chrono::Duration::zero())
                    .unwrap_or_else(default_timer),
                Some(s) => duration_arg(s)?,
            };
            let stopped = store.active_timer().copied();
            let active = store.start_timer(id, length, now)?;
            store.save()?;
            if let Some(prev) = stopped.filter(|p| p.todo != id) {
                if !cli.json {
                    println!(
                        "stopped #{} after {}",
                        prev.todo,
                        format_duration(prev.elapsed(now))
                    );
                }
            }
            print_timer(&store, Some(&active), now, cli.json)?;
        }
        Command::Stop => {
            let todo = store.stop_timer(now);
            store.save()?;
            match (todo, cli.json) {
                (Some(todo), true) => print_one(&todo, true)?,
                (Some(todo), false) => println!(
                    "stopped #{} {} after {} ({} total)",
                    todo.id,
                    todo.title,
                    format_duration(
                        todo.sessions
                            .last()
                            .map_or_else(Default::default, |s| s.duration())
                    ),
                    format_duration(todo.time_spent())
                ),
                (None, true) => println!("null"),
                (None, false) => println!("no timer running"),
            }
        }
        Command::Pause => {
            let active = store.pause_timer(now);
            store.save()?;
            print_timer(&store, active.as_ref(), now, cli.json)?;
        }
        Command::Resume => {
            let active = store.resume_timer(now);
            store.save()?;
            print_timer(&store, active.as_ref(), now, cli.json)?;
        }
        Command::Timer => print_timer(&store, store.active_timer(), now, cli.json)?,
        Command::Time(args) => {
            let id = args.id;
            let count = store
                .get(id)
                .with_context(|| format!("no todo with id {id}"))?
                .sessions
                .len();
            // Listed newest first, numbered from 1.
            let index = count
                .checked_sub(args.session)
                .filter(|_| args.session > 0)
                .ok_or_else(|| anyhow!("#{id} has no session {}", args.session));
            let changed = if let Some(add) = args.add.as_deref() {
                Some(store.add_session(id, duration_arg(add)?, now)?)
            } else if args.rm {
                Some(store.delete_session(id, index?)?)
            } else if let Some(length) = args.length.as_deref() {
                Some(store.set_session_length(id, index?, duration_arg(length)?)?)
            } else {
                None
            };
            if changed.is_some() {
                store.save()?;
            }
            let todo = store.get(id).expect("checked above");
            if cli.json {
                print_one(todo, true)?;
            } else {
                print_sessions(todo, usize::MAX);
            }
        }
    }
    Ok(())
}

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn day_arg(s: &str) -> Result<NaiveDate> {
    parse_day(s, today())
        .ok_or_else(|| anyhow!("unknown day {s:?}: try today, tomorrow, fri or YYYY-MM-DD"))
}

fn duration_arg(s: &str) -> Result<chrono::Duration> {
    parse_duration(s).ok_or_else(|| anyhow!("unknown duration {s:?}: try 25, 34m or 1h30m"))
}

/// `18:42 left of 25m`, or `+01:12 over 25m` once the block is up, with
/// a note while paused.
fn timer_state(active: &ActiveSession, now: DateTime<Utc>) -> String {
    let planned = format_duration(active.planned());
    let clock = format_timer(active, now);
    let state = if active.is_over(now) {
        format!("{clock} over {planned}")
    } else {
        format!("{clock} left of {planned}")
    };
    if active.is_auto_paused(now) {
        format!("{state} (paused itself, left running · agentodo resume or stop)")
    } else if active.is_paused(now) {
        format!("{state} (paused · agentodo resume)")
    } else {
        state
    }
}

/// The newest `limit` sessions, numbered newest first as `time --session`
/// takes them.
fn print_sessions(t: &Todo, limit: usize) {
    if t.sessions.is_empty() {
        println!("#{} has no recorded time", t.id);
        return;
    }
    for (n, s) in t.sessions.iter().rev().enumerate().take(limit) {
        let (start, end) = (s.start.with_timezone(&Local), s.end.with_timezone(&Local));
        let over = s
            .overtime()
            .map(|o| format!("  +{} over", format_duration(o)))
            .unwrap_or_default();
        let paused = match s.paused_secs {
            0 => String::new(),
            _ => format!("  ({} paused)", format_duration(s.paused())),
        };
        println!(
            "  {:>3}  {}  {}–{}  {:>6}{over}{paused}",
            n + 1,
            start.format("%a %-d %b"),
            start.format("%H:%M"),
            end.format("%H:%M"),
            format_duration(s.duration()),
        );
    }
    let earlier = t.sessions.len().saturating_sub(limit);
    if earlier > 0 {
        println!("       +{earlier} earlier (agentodo time {})", t.id);
    }
}

fn print_timer(
    store: &Store,
    active: Option<&ActiveSession>,
    now: DateTime<Utc>,
    json: bool,
) -> Result<()> {
    let Some(active) = active else {
        println!("{}", if json { "null" } else { "no timer running" });
        return Ok(());
    };
    if json {
        let value = serde_json::json!({
            "todo": active.todo,
            "started_at": active.started_at,
            "planned_secs": active.planned_secs,
            "ends_at": active.ends_at(now),
            "elapsed_secs": active.elapsed(now).num_seconds(),
            "remaining_secs": active.remaining(now).num_seconds(),
            "overtime_secs": active.overtime(now).num_seconds(),
            "paused": active.is_paused(now),
            "auto_paused": active.is_auto_paused(now),
            "paused_at": active.paused_at.or(active.auto_pause_at().filter(|at| *at <= now)),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    let title = store.get(active.todo).map_or("", |t| t.title.as_str());
    println!("⏱ #{} {}  {}", active.todo, title, timer_state(active, now));
    Ok(())
}

fn print_one(todo: &Todo, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(todo)?);
    } else {
        print_detail(todo);
    }
    Ok(())
}

fn print_list(todos: &[Todo], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(todos)?);
        return Ok(());
    }
    if todos.is_empty() {
        println!("No matching todos.");
        return Ok(());
    }
    // Compact table: id, status mark, priority, day, title, project/tags.
    println!("{:>4}  {:1}  {:<6}  {:<6}  TITLE", "ID", "", "PRIO", "WHEN");
    for t in todos {
        let mark = match t.status {
            Status::Open => " ",
            Status::Done => "x",
        };
        let mut meta = String::new();
        if let Some(p) = &t.project {
            meta.push_str(&format!("  @{p}"));
        }
        for tag in &t.tags {
            meta.push_str(&format!("  #{tag}"));
        }
        if let Some(plan) = t.plan() {
            meta.push_str(&format!(
                "  ~{}/{}",
                format_duration(t.time_spent()),
                format_duration(plan)
            ));
        }
        println!(
            "{:>4}  [{}] {:<6}  {:<6}  {}{}",
            t.id,
            mark,
            priority_label(t.priority),
            t.scheduled
                .map(|d| d.format("%a %-d").to_string())
                .unwrap_or_default(),
            t.title,
            meta
        );
    }
    Ok(())
}

fn print_projects(projects: &[ProjectSummary], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(projects)?);
        return Ok(());
    }
    if projects.is_empty() {
        println!("No projects yet.");
        return Ok(());
    }
    // Named projects render as @name to match print_list; the unassigned
    // bucket is (none). Width tracks the longest label so names never wrap.
    let labels: Vec<String> = projects.iter().map(project_label).collect();
    let width = labels.iter().map(String::len).max().unwrap_or(0).max(7);
    println!("{:<width$}  OPEN  DONE", "PROJECT", width = width);
    for (p, label) in projects.iter().zip(&labels) {
        println!(
            "{:<width$}  {:>4}  {:>4}",
            label,
            p.open,
            p.done,
            width = width
        );
    }
    Ok(())
}

fn project_label(p: &ProjectSummary) -> String {
    match &p.project {
        Some(name) => format!("@{name}"),
        None => "(none)".to_string(),
    }
}

fn print_detail(t: &Todo) {
    println!("#{}  {}", t.id, t.title);
    println!(
        "  status:   {}",
        match t.status {
            Status::Open => "open",
            Status::Done => "done",
        }
    );
    println!("  priority: {}", priority_label(t.priority));
    if let Some(p) = &t.project {
        println!("  project:  {p}");
    }
    if !t.tags.is_empty() {
        println!("  tags:     {}", t.tags.join(", "));
    }
    if let Some(n) = &t.notes {
        println!("  notes:    {n}");
    }
    if let Some(d) = t.scheduled {
        println!("  on:       {}", d.format("%a %Y-%m-%d"));
    }
    if let Some(plan) = t.plan() {
        let spent = t.time_spent();
        let status = if spent > plan {
            format!("{} over", format_duration(spent - plan))
        } else {
            format!("{} left", format_duration(plan - spent))
        };
        println!(
            "  plan:     {} · {} spent · {status}",
            format_duration(plan),
            format_duration(spent)
        );
    }
    if !t.sessions.is_empty() {
        let n = t.sessions.len();
        println!(
            "  time:     {} ({n} session{})",
            format_duration(t.time_spent()),
            if n == 1 { "" } else { "s" }
        );
    }
    println!("  created:  {}", t.created_at.to_rfc3339());
    if let Some(c) = t.completed_at {
        println!("  done at:  {}", c.to_rfc3339());
    }
}

fn priority_label(p: Priority) -> &'static str {
    match p {
        Priority::Low => "low",
        Priority::Medium => "med",
        Priority::High => "high",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Catches duplicate short/long flags, bad defaults and conflicting groups,
    /// which clap otherwise only reports by panicking at runtime.
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
