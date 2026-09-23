use std::path::PathBuf;

use agentodo_core::{
    Actor, Filter, NewTodo, Priority, ProjectSummary, SortBy, Source, Status, Store, Todo,
    TodoPatch,
};
use anyhow::{Context, Result};
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

    match cli.command {
        Command::Add(args) => {
            let todo = store.add(NewTodo {
                title: args.title,
                priority: args.priority.into(),
                project: args.project,
                tags: args.tags,
                notes: args.notes,
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
                sort: args.sort.into(),
            };
            let todos = store.list(&filter);
            print_list(&todos, cli.json)?;
        }
        Command::Show { id } => {
            let todo = store.get(id).with_context(|| format!("no todo with id {id}"))?;
            print_one(todo, cli.json)?;
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
                project: args.project.map(|p| if p.is_empty() { None } else { Some(p) }),
                notes: args.notes.map(|n| if n.is_empty() { None } else { Some(n) }),
                add_tags: args.add_tag,
                remove_tags: args.remove_tag,
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
    }
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
    // Compact table: id, status mark, priority, title, project/tags.
    println!("{:>4}  {:1}  {:<6}  TITLE", "ID", "", "PRIO");
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
        println!(
            "{:>4}  [{}] {:<6}  {}{}",
            t.id,
            mark,
            priority_label(t.priority),
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
