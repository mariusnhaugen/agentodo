//! A vim-flavoured terminal UI for agentodo.
//!
//! Every edit goes straight to the store (reload → change → save) and the
//! store file is polled, so todos added by agents over MCP show up live.

mod app;
mod line;
mod repo;
mod ui;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use agentodo_core::Store;
use anyhow::{bail, Context, Result};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::DefaultTerminal;

use app::{App, Effect};
use repo::Repo;

/// Browse and edit todos with vim keys.
#[derive(Debug, Parser)]
#[command(name = "agentodo-tui", version, about)]
struct Cli {
    /// Path to the JSON store. Overrides $AGENTODO_STORE and the default location.
    #[arg(long, value_name = "PATH")]
    store: Option<PathBuf>,
}

/// How often to check the store file for writes from other processes.
const POLL: Duration = Duration::from_secs(1);

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = Store::resolve_path(cli.store).context("determining default store path")?;
    let mut app = App::new(Repo::new(path))?;

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut last_poll = Instant::now();
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match app.handle_key(key) {
                        Effect::None => {}
                        Effect::Quit => return Ok(()),
                        Effect::EditNotes { id, notes } => {
                            let edited = with_suspended(terminal, || edit_in_editor(&notes))?;
                            match edited {
                                Ok(text) => app.finish_notes(id, text),
                                Err(e) => app.error(format!("{e:#}")),
                            }
                        }
                    }
                }
            }
        }

        if last_poll.elapsed() >= POLL {
            app.refresh_if_changed();
            last_poll = Instant::now();
        }
    }
}

/// Hands the terminal back to the shell while `f` runs.
fn with_suspended<T>(terminal: &mut DefaultTerminal, f: impl FnOnce() -> T) -> Result<T> {
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    let out = f();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    Ok(out)
}

/// Opens `text` in `$VISUAL`/`$EDITOR` (falling back to `vi`) and returns
/// the saved result.
fn edit_in_editor(text: &str) -> Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let path = std::env::temp_dir().join(format!("agentodo-notes-{}.md", std::process::id()));
    {
        let mut file = std::fs::File::create(&path)?;
        file.write_all(text.as_bytes())?;
    }

    // The editor setting may carry arguments, e.g. `code --wait`.
    let mut parts = editor.split_whitespace();
    let program = parts.next().context("empty $EDITOR")?;
    let status = Command::new(program)
        .args(parts)
        .arg(&path)
        .status()
        .with_context(|| format!("running {editor}"))?;
    let result = std::fs::read_to_string(&path);
    std::fs::remove_file(&path).ok();
    if !status.success() {
        bail!("{editor} exited with {status}; notes unchanged");
    }
    Ok(result?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
