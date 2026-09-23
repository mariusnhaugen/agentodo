use agentodo_core::{Priority, SortBy, Status, Todo};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Row,
    Table, Wrap,
};
use ratatui::Frame;

use crate::app::{App, InputKind, Mode};
use crate::line::LineEdit;

const DIM: Style = Style::new().fg(Color::DarkGray);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, list, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    draw_list(frame, app, list);
    draw_footer(frame, app, footer);

    match &app.mode {
        Mode::Help => draw_help(frame),
        Mode::Detail => draw_detail(frame, app),
        Mode::Picker { .. } => draw_picker(frame, app),
        _ => {}
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let sort = match app.sort {
        SortBy::Priority => "priority",
        SortBy::Created => "created",
    };
    let done = if app.show_done { "open + done" } else { "open" };
    let left = Line::from(vec![
        " agentodo".bold(),
        Span::styled(" — ", DIM),
        Span::raw(app.project.label().to_owned()).cyan(),
        Span::styled(format!(" · {done} · sort: {sort}"), DIM),
    ]);
    let (open, done) = app.counts();
    let right = Line::from(format!(" [{open} open / {done} done] ")).right_aligned();
    // Give the counts their full width; the description truncates first.
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right.width() as u16),
    ])
    .areas(area);
    frame.render_widget(left, left_area);
    frame.render_widget(right, right_area);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(DIM);
    let inner = block.inner(area);
    app.page = inner.height as usize;

    // A row being typed in insert mode: where it goes and what it says.
    let (new_row, editing) = match &app.mode {
        Mode::Input {
            kind: InputKind::NewTodo { above },
            line,
        } => {
            let at = if app.rows.is_empty() || *above {
                app.cursor
            } else {
                app.cursor + 1
            };
            (Some((at, line)), None)
        }
        Mode::Input {
            kind: InputKind::EditTitle(id),
            line,
        } => (None, Some((*id, line))),
        _ => (None, None),
    };

    if app.rows.is_empty() && new_row.is_none() {
        let hint = Paragraph::new(Line::from(vec![
            Span::styled("No todos here — press ", DIM),
            "o".bold(),
            Span::styled(" to add one", DIM),
        ]))
        .block(block);
        frame.render_widget(hint, area);
        return;
    }

    let project_width = app
        .rows
        .iter()
        .filter_map(|t| t.project.as_deref())
        .map(|p| p.chars().count())
        .max()
        .unwrap_or(1)
        .clamp(1, 16) as u16;
    let tags_width = app
        .rows
        .iter()
        .map(|t| tags_text(t).chars().count())
        .max()
        .unwrap_or(0)
        .min(24) as u16;
    let id_width = app
        .rows
        .iter()
        .map(|t| t.id.to_string().len())
        .max()
        .unwrap_or(1)
        .max(3) as u16;

    let search = app.search.as_deref();
    let mut rows: Vec<Row> = app
        .rows
        .iter()
        .map(|t| {
            let title = match editing {
                Some((id, line)) if id == t.id => input_line(line),
                _ => title_line(t, search),
            };
            todo_row(t, title)
        })
        .collect();

    let selected = match new_row {
        Some((at, line)) => {
            rows.insert(
                at,
                Row::new(vec![
                    Cell::from("+".green()),
                    Cell::from(Line::from("new").right_aligned()).style(DIM),
                    Cell::from(input_line(line)),
                    Cell::from(""),
                    Cell::from(""),
                ]),
            );
            at
        }
        None => app.cursor,
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(id_width),
            Constraint::Fill(1),
            Constraint::Length(project_width),
            Constraint::Length(tags_width),
        ],
    )
    .block(block)
    .column_spacing(1)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);

    app.table.select(Some(selected));
    frame.render_stateful_widget(table, area, &mut app.table);
}

fn todo_row<'a>(t: &'a Todo, title: Line<'a>) -> Row<'a> {
    let done = t.status == Status::Done;
    let marker = if done {
        Span::raw("✓")
    } else {
        match t.priority {
            Priority::High => "●".red(),
            Priority::Medium => "◐".yellow(),
            Priority::Low => "○".dark_gray(),
        }
    };
    let project = match t.project.as_deref().filter(|p| !p.is_empty()) {
        Some(p) if done => Span::raw(p),
        Some(p) => Span::raw(p).cyan(),
        None => Span::styled("—", DIM),
    };
    let tags = if done {
        Span::raw(tags_text(t))
    } else {
        Span::raw(tags_text(t)).magenta()
    };

    let row = Row::new(vec![
        Cell::from(marker),
        Cell::from(Line::from(t.id.to_string()).right_aligned()).style(DIM),
        Cell::from(title),
        Cell::from(project),
        Cell::from(tags),
    ]);
    if done {
        row.style(DIM)
    } else {
        row
    }
}

fn tags_text(t: &Todo) -> String {
    t.tags
        .iter()
        .map(|tag| format!("#{tag}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title with every case-insensitive match of the search term highlighted.
fn title_line<'a>(t: &'a Todo, search: Option<&str>) -> Line<'a> {
    let style = if t.status == Status::Done {
        Style::new().add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::new()
    };
    let Some(term) = search.filter(|s| !s.is_empty()) else {
        return Line::from(Span::styled(t.title.as_str(), style));
    };

    // Lowercasing can change byte lengths for some scripts; only highlight
    // when offsets line up, otherwise fall back to the plain title.
    let lower = t.title.to_lowercase();
    if lower.len() != t.title.len() {
        return Line::from(Span::styled(t.title.as_str(), style));
    }
    let term = term.to_lowercase();
    let mut spans = Vec::new();
    let mut last = 0;
    for (start, m) in lower.match_indices(&term) {
        spans.push(Span::styled(&t.title[last..start], style));
        spans.push(Span::styled(
            &t.title[start..start + m.len()],
            style.black().on_yellow(),
        ));
        last = start + m.len();
    }
    spans.push(Span::styled(&t.title[last..], style));
    Line::from(spans)
}

/// A line of typed text with a block cursor.
fn input_line(line: &LineEdit) -> Line<'_> {
    let (before, under, after) = line.split();
    let cursor = Style::new().add_modifier(Modifier::REVERSED);
    Line::from(vec![
        Span::raw(before),
        Span::styled(under.map_or(" ".to_string(), String::from), cursor),
        Span::raw(after),
    ])
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    if let Mode::Input { kind, line } = &app.mode {
        let prompt = match kind {
            InputKind::Command => Some(":"),
            InputKind::Search { .. } => Some("/"),
            InputKind::SetProject(_) => Some("project: "),
            _ => None,
        };
        if let Some(prompt) = prompt {
            let mut spans = vec![Span::raw(prompt)];
            spans.extend(input_line(line).spans);
            frame.render_widget(Line::from(spans), area);
            return;
        }
    }

    let (label, color) = match &app.mode {
        Mode::Input { .. } => (" INSERT ", Color::Green),
        Mode::Picker { .. } => (" PROJECT ", Color::Cyan),
        _ => (" NORMAL ", Color::Blue),
    };
    let mut spans = vec![
        Span::styled(label, Style::new().fg(Color::Black).bg(color).bold()),
        " ".into(),
    ];
    match (&app.message, &app.mode) {
        (Some(m), _) if m.error => spans.push(Span::raw(m.text.as_str()).red()),
        (Some(m), _) => spans.push(Span::raw(m.text.as_str())),
        (None, Mode::Input { .. }) => spans.push(Span::styled(
            "Enter/Esc save · !h !m !l priority · +project · #tag",
            DIM,
        )),
        (None, _) => spans.push(Span::styled(
            "j/k move · o add · dd delete · x done · u undo · p project · ? help",
            DIM,
        )),
    }
    frame.render_widget(Line::from(spans), area);

    let showcmd = app.pending.display();
    if !showcmd.is_empty() {
        frame.render_widget(Line::from(format!("{showcmd}  ")).right_aligned(), area);
    }
}

fn popup(frame: &mut Frame, width: u16, height: u16, title: &str) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(frame.area());
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::new().cyan());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

const HELP: &[(&str, &str)] = &[
    ("j k ↓ ↑", "move (takes a count: 5j)"),
    ("gg G  5G", "top / bottom / row 5"),
    ("C-d C-u", "half page down / up"),
    ("o O", "add todo below / above"),
    ("i  cc", "edit title / retype title"),
    ("dd  3dd", "delete (count deletes several)"),
    ("x", "toggle done"),
    ("+ -", "raise / lower priority"),
    ("gp", "set project"),
    ("e", "edit notes in $EDITOR"),
    ("u  C-r", "undo / redo"),
    ("/  n N", "search titles / next / previous"),
    ("p", "filter by project"),
    (".", "show / hide done"),
    ("s", "toggle sort: priority / created"),
    ("Enter", "details"),
    (":", ":q  :project [name]  :sort priority|created"),
    ("q  ZZ", "quit"),
    ("", ""),
    ("new todo", "title !h +project #tag"),
];

fn draw_help(frame: &mut Frame) {
    let inner = popup(frame, 62, HELP.len() as u16 + 2, "keys");
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(keys, what)| {
            Line::from(vec![
                format!("  {keys:<10}").bold().yellow(),
                Span::raw(*what),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_detail(frame: &mut Frame, app: &App) {
    let Some(t) = app.current() else { return };
    let field = |name: &'static str, value: String| {
        Line::from(vec![
            Span::styled(format!("{name:>10}  "), DIM),
            Span::raw(value),
        ])
    };
    let mut lines = vec![
        Line::from(t.title.as_str()).bold(),
        Line::default(),
        field("id", format!("#{}", t.id)),
        field("status", format!("{:?}", t.status).to_lowercase()),
        field("priority", format!("{:?}", t.priority).to_lowercase()),
        field("project", t.project.clone().unwrap_or_else(|| "—".into())),
        field(
            "tags",
            if t.tags.is_empty() {
                "—".into()
            } else {
                tags_text(t)
            },
        ),
        field(
            "created",
            t.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        ),
        field(
            "updated",
            t.updated_at.format("%Y-%m-%d %H:%M UTC").to_string(),
        ),
    ];
    if let Some(done) = t.completed_at {
        lines.push(field(
            "completed",
            done.format("%Y-%m-%d %H:%M UTC").to_string(),
        ));
    }
    lines.push(Line::default());
    match t.notes.as_deref() {
        Some(notes) => lines.extend(Text::from(notes).lines),
        None => lines.push(Line::styled("no notes — press e to write some", DIM)),
    }

    let area = frame.area();
    let width = area.width.saturating_sub(8).min(80);
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let inner = popup(frame, width, height, "todo");
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_picker(frame: &mut Frame, app: &App) {
    let Mode::Picker { items, selected } = &app.mode else {
        return;
    };
    let rows: Vec<ListItem> = items
        .iter()
        .map(|p| {
            let (open, done) = app.project_counts(p);
            ListItem::new(Line::from(vec![
                Span::raw(format!("{:<20}", p.label())),
                Span::styled(format!("{open:>3} open {done:>3} done"), DIM),
            ]))
        })
        .collect();
    let height = (rows.len() as u16 + 2).min(frame.area().height.saturating_sub(2));
    let inner = popup(frame, 42, height, "project");
    let list = List::new(rows)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(
        list,
        inner,
        &mut ListState::default().with_selected(Some(*selected)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{press, TempStore};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn render(app: &mut App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// The screen row whose text contains `needle`.
    fn find_row(buf: &Buffer, needle: &str) -> u16 {
        (0..buf.area.height)
            .find(|&y| row_text(buf, y).contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not on screen"))
    }

    #[test]
    fn renders_header_rows_and_highlights_cursor() {
        let ts = TempStore::new();
        ts.seed(&["buy milk !l +home #errand", "ship release !h +work #urgent"]);
        let mut app = ts.app();
        let buf = render(&mut app, 70, 8);

        assert!(row_text(&buf, 0).contains("agentodo — all projects"));
        assert!(row_text(&buf, 0).contains("[2 open / 0 done]"));

        let ship = find_row(&buf, "ship release");
        assert!(row_text(&buf, ship).starts_with("▶ ●   2 ship release"));
        assert!(row_text(&buf, ship).contains("work"));
        assert!(row_text(&buf, ship).contains("#urgent"));
        assert!(buf[(4, ship)].modifier.contains(Modifier::REVERSED));

        let milk = find_row(&buf, "buy milk");
        assert!(!buf[(4, milk)].modifier.contains(Modifier::REVERSED));
        assert!(row_text(&buf, 7).contains("NORMAL"));
    }

    #[test]
    fn done_rows_are_dimmed_and_hideable() {
        let ts = TempStore::new();
        ts.seed(&["finished", "pending !l"]);
        let mut app = ts.app();
        press(&mut app, "x"); // cursor starts on "finished" (medium beats low)
        let buf = render(&mut app, 60, 8);
        let y = find_row(&buf, "finished");
        assert!(row_text(&buf, y).contains('✓'));
        let col = (0..60).find(|&x| buf[(x, y)].symbol() == "f").unwrap();
        assert_eq!(buf[(col, y)].fg, Color::DarkGray);
        assert!(buf[(col, y)].modifier.contains(Modifier::CROSSED_OUT));

        press(&mut app, ".");
        let buf = render(&mut app, 60, 8);
        assert!((0..8).all(|y| !row_text(&buf, y).contains("finished")));
    }

    #[test]
    fn insert_row_and_prompts_render() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();

        press(&mut app, "onew thing");
        let buf = render(&mut app, 50, 8);
        let y = find_row(&buf, "new thing");
        assert!(row_text(&buf, y).contains("new new thing"));
        assert!(y > find_row(&buf, " a "), "o opens below the cursor");
        assert!(row_text(&buf, 7).contains("INSERT"));

        press(&mut app, "<Esc>:sort");
        let buf = render(&mut app, 50, 8);
        assert!(row_text(&buf, 7).starts_with(":sort"));
    }

    #[test]
    fn header_keeps_counts_visible_when_narrow() {
        let ts = TempStore::new();
        ts.seed(&["a"]);
        let mut app = ts.app();
        let buf = render(&mut app, 40, 6);
        assert!(row_text(&buf, 0).trim_end().ends_with("[1 open / 0 done]"));
    }

    #[test]
    fn empty_store_shows_hint_and_overlays_render() {
        let ts = TempStore::new();
        let mut app = ts.app();
        let buf = render(&mut app, 50, 8);
        assert!(find_row(&buf, "press o to add one") > 0);

        press(&mut app, "?");
        let buf = render(&mut app, 70, 26);
        find_row(&buf, " keys ");
        find_row(&buf, "toggle done");
    }

    #[test]
    #[ignore = "prints a frame for eyeballing: cargo test -p agentodo-tui -- --ignored --nocapture"]
    fn print_frame() {
        let ts = TempStore::new();
        ts.seed(&[
            "buy milk !l +home #errand",
            "write docs",
            "ship release !h +work #urgent #release",
            "answer email +work",
        ]);
        let mut app = ts.app();
        press(&mut app, "Gx/re<CR>");
        let buf = render(&mut app, 72, 10);
        for y in 0..buf.area.height {
            println!("{}", row_text(&buf, y));
        }
    }
}
