use agentodo_core::{
    format_countdown, format_duration, format_timer, ActiveSession, EntryKind, LogEntry, Priority,
    SortBy, Source, Status, Todo,
};
use chrono::{Datelike, Local, NaiveDate};
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, List, ListItem, ListState, Padding,
    Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;

use crate::app::{App, Card, InputKind, Mode, View, WEEK_COLUMNS};
use crate::line::LineEdit;

const DIM: Style = Style::new().fg(Color::DarkGray);

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, list, player, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(player_height(app, frame.area().height)),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_player(frame, app, player);

    if let Mode::Log { .. } = app.mode {
        draw_log_header(frame, app, header);
        draw_log(frame, app, list);
    } else if app.view == View::Week {
        draw_header(frame, app, header);
        let [board, strip] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(list);
        draw_week(frame, app, board);
        draw_focus_strip(frame, app, strip);
    } else {
        draw_header(frame, app, header);
        draw_list(frame, app, list);
    }
    draw_footer(frame, app, footer);

    match &app.mode {
        Mode::Help => draw_help(frame),
        Mode::Detail { id, selected } => draw_detail(frame, app, *id, *selected),
        Mode::Input {
            kind: InputKind::SessionLength { id, index },
            ..
        } => {
            let count = app
                .all_todos()
                .iter()
                .find(|t| t.id == *id)
                .map_or(0, |t| t.sessions.len());
            draw_detail(frame, app, *id, count.saturating_sub(index + 1))
        }
        Mode::Input {
            kind: InputKind::AddSession(id),
            ..
        } => draw_detail(frame, app, *id, 0),
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
    let mut left = Line::from(vec![
        " agentodo".bold(),
        Span::styled(" — ", DIM),
        Span::raw(app.project.label().to_owned()).cyan(),
        Span::styled(format!(" · {done} · sort: {sort}"), DIM),
    ]);
    if app.view == View::Week {
        left.spans.insert(
            1,
            Span::raw(format!(" week of {}", week_range(app.week))).bold(),
        );
    }
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

    let timed = app
        .timer
        .is_some_and(|a| app.rows.iter().any(|t| t.id == a.todo));
    let when_width = if timed || app.rows.iter().any(|t| t.scheduled.is_some()) {
        6
    } else {
        0
    };

    let plans: Vec<Span> = app.rows.iter().map(|t| plan_text(app, t)).collect();
    let plan_width = plans.iter().map(Span::width).max().unwrap_or(0) as u16;

    let search = app.search.as_deref();
    let mut rows: Vec<Row> = app
        .rows
        .iter()
        .zip(plans)
        .map(|(t, plan)| {
            let title = match editing {
                Some((id, line)) if id == t.id => input_line(line),
                _ => title_line(t, search),
            };
            let when = match countdown(app, t.id) {
                Some(clock) => clock,
                None => when_text(t, app.today),
            };
            let timed = app
                .timer
                .filter(|a| a.todo == t.id)
                .map(|a| timer_color(&a, app.now()));
            todo_row(t, title, app.changed.contains(&t.id), when, plan, timed)
        })
        .collect();

    let selected = match new_row {
        Some((at, line)) => {
            rows.insert(
                at,
                Row::new(vec![
                    Cell::from(""),
                    Cell::from("+".green()),
                    Cell::from(Line::from("new").right_aligned()).style(DIM),
                    Cell::from(input_line(line)),
                    Cell::from(""),
                    Cell::from(""),
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
            Constraint::Length(1),
            Constraint::Length(id_width),
            Constraint::Fill(1),
            Constraint::Length(when_width),
            Constraint::Length(plan_width),
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

/// `45m/2h`: time spent (a running timer included) against the plan, yellow
/// once over it. Empty without a plan.
fn plan_text(app: &App, t: &Todo) -> Span<'static> {
    let Some(plan) = t.plan() else {
        return Span::raw("");
    };
    let spent = t.time_spent() + app.running_on(t.id);
    let text = format!("{}/{}", format_duration(spent), format_duration(plan));
    if spent > plan {
        Span::raw(text).yellow()
    } else {
        Span::styled(text, DIM)
    }
}

/// One list row. `changed` marks a todo someone else touched since you last
/// looked; `when` is its day, or the countdown while its timer runs; `plan`
/// is time spent against its plan, and `timed` the timer's colour, which
/// badges the id so the row stands out.
fn todo_row<'a>(
    t: &'a Todo,
    title: Line<'a>,
    changed: bool,
    when: Span<'a>,
    plan: Span<'a>,
    timed: Option<Color>,
) -> Row<'a> {
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

    let change = if changed {
        "•".cyan().bold()
    } else {
        Span::raw("")
    };
    let row = Row::new(vec![
        Cell::from(change),
        Cell::from(marker),
        Cell::from(Line::from(t.id.to_string()).right_aligned()).style(match timed {
            Some(color) => Style::new().fg(Color::Black).bg(color).bold(),
            None => DIM,
        }),
        Cell::from(title),
        Cell::from(when),
        Cell::from(plan),
        Cell::from(project),
        Cell::from(tags),
    ]);
    if done {
        row.style(DIM)
    } else {
        row
    }
}

/// A scheduled day, short: `Fri` within this week, `Oct 3` otherwise.
fn when_text(t: &Todo, today: NaiveDate) -> Span<'static> {
    let Some(day) = t.scheduled else {
        return Span::raw("");
    };
    let this_week = agentodo_core::week_start(day) == agentodo_core::week_start(today);
    let text = if this_week {
        day.format("%a").to_string()
    } else {
        day.format("%b %-d").to_string()
    };
    if day == today {
        Span::raw(text).yellow()
    } else {
        Span::raw(text).blue()
    }
}

/// `id`'s timer clock if it's the one running: time left, or `+01:12` of
/// overtime once its block is up.
fn countdown_text(app: &App, id: u64) -> Option<String> {
    let timer = app.timer.filter(|t| t.todo == id)?;
    Some(format_timer(&timer, app.now()))
}

/// [`countdown_text`] in the timer's colour.
fn countdown(app: &App, id: u64) -> Option<Span<'static>> {
    let text = countdown_text(app, id)?;
    let color = timer_color(&app.timer?, app.now());
    Some(Span::styled(text, Style::new().fg(color).bold()))
}

/// Overtime past which a timer looks forgotten rather than stretched.
const FORGOTTEN: chrono::Duration = chrono::Duration::minutes(15);

/// Green while in the block, yellow in overtime, red once it looks
/// forgotten, grey while paused.
fn timer_color(timer: &ActiveSession, now: chrono::DateTime<chrono::Utc>) -> Color {
    if timer.is_paused(now) {
        Color::DarkGray
    } else if timer.overtime(now) >= FORGOTTEN {
        Color::Red
    } else if timer.is_over(now) {
        Color::Yellow
    } else {
        Color::Green
    }
}

// ---- player -------------------------------------------------------------------

/// Widest the docked player grows; wider terminals centre it.
const PLAYER_WIDTH: u16 = 72;
/// Shorter screens get the one-line player so the list keeps its rows.
const PLAYER_MIN_SCREEN: u16 = 20;
const PLAYER_KEYS: &str = " P pause · S stop · gT jump ";

/// Rows the player docks in above the footer: none without a timer, a
/// bordered card, or a single line on a short screen.
fn player_height(app: &App, screen: u16) -> u16 {
    match app.timer {
        None => 0,
        Some(_) if screen >= PLAYER_MIN_SCREEN => 4,
        Some(_) => 1,
    }
}

/// The running timer, docked like a music player's now-playing bar:
///
/// ```text
/// ╭ focus ───────────────────────────────────────────────────────╮
/// │ ▶  #3 write release notes                  work · ends 09:25 │
/// │ 06:18 ━━━━━━━━━━━━━━━──────────────────────────────── -18:42 │
/// ╰──────────────────────────────────────────── S stop · gT jump ╯
/// ```
///
/// On a short screen it folds into one line: `▶ 18:42 #3 title ━━──`.
fn draw_player(frame: &mut Frame, app: &App, area: Rect) {
    let Some(timer) = app.timer else { return };
    if area.height == 0 {
        return;
    }
    let now = app.now();
    let color = timer_color(&timer, now);
    let tint = Style::new().fg(color).bold();
    let todo = app.all_todos().iter().find(|t| t.id == timer.todo);
    let title = todo.map_or("", |t| t.title.as_str());
    let planned = timer.planned_secs.max(1) as f64;
    let elapsed = timer.elapsed(now);
    let ratio = (elapsed.num_seconds().max(0) as f64 / planned).min(1.0);
    let paused = timer.is_paused(now);
    // The clock already says ⏸ while paused.
    let icon = if paused { "" } else { "▶ " };

    if area.height < 4 {
        let badge = Style::new().fg(Color::Black).bg(color).bold();
        let mut line = Line::from(vec![
            Span::styled(format!(" {icon}{} ", format_timer(&timer, now)), badge),
            Span::styled(format!(" #{} ", timer.todo), DIM),
            Span::raw(clip(title, 32)),
            " ".into(),
        ]);
        let room = (area.width as usize).saturating_sub(line.width() + PLAYER_KEYS.len());
        if room >= 8 {
            line.spans.extend(progress(ratio, room, color));
        }
        line.spans.push(Span::styled(PLAYER_KEYS, DIM));
        frame.render_widget(line, area);
        return;
    }

    let [card] = Layout::horizontal([Constraint::Length(area.width.min(PLAYER_WIDTH))])
        .flex(Flex::Center)
        .areas(area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color))
        .title(Span::styled(" focus ", tint))
        .title_bottom(Line::styled(PLAYER_KEYS, DIM).right_aligned())
        .padding(Padding::horizontal(1));
    let inner = block.inner(card);
    frame.render_widget(block, card);
    let [top, bottom] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    // What's playing, with the project and when the block ends (or how far
    // past it we are) on the right.
    let clock = if timer.is_over(now) {
        format!("+{}", format_countdown(timer.overtime(now)))
    } else {
        format!("-{}", format_countdown(timer.remaining(now)))
    };
    let mut right = Line::default().right_aligned();
    if let Some(p) = todo
        .and_then(|t| t.project.as_deref())
        .filter(|p| !p.is_empty())
    {
        right.spans.push(Span::raw(p.to_owned()).cyan());
        right.spans.push(Span::styled(" · ", DIM));
    }
    if timer.is_auto_paused(now) {
        right
            .spans
            .push(Span::styled("paused itself · P resumes", tint));
    } else if paused {
        right.spans.push(Span::styled("paused · P resumes", tint));
    } else if timer.is_over(now) {
        right
            .spans
            .push(Span::styled(format!("{clock} over"), tint));
    } else if let Some(ends) = timer.ends_at(now) {
        let ends = ends.with_timezone(&Local).format("%H:%M");
        right.spans.push(Span::styled(format!("ends {ends}"), DIM));
    }
    let [title_area, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right.width() as u16 + 1),
    ])
    .areas(top);
    frame.render_widget(
        Line::from(vec![
            Span::styled(if paused { "⏸  " } else { "▶  " }, tint),
            Span::styled(format!("#{} ", timer.todo), DIM),
            Span::raw(title.to_owned()).bold(),
        ]),
        title_area,
    );
    frame.render_widget(right, right_area);

    // Elapsed, the bar, then time left or the overtime.
    let elapsed = format_countdown(elapsed);
    let room = (bottom.width as usize).saturating_sub(elapsed.len() + clock.len() + 2);
    let mut bar = vec![Span::raw(elapsed), " ".into()];
    bar.extend(progress(ratio, room, color));
    bar.push(" ".into());
    bar.push(Span::styled(clock, tint));
    frame.render_widget(Line::from(bar), bottom);
}

/// A `width`-cell bar with `ratio` of it filled in `color`.
fn progress(ratio: f64, width: usize, color: Color) -> [Span<'static>; 2] {
    let filled = (ratio * width as f64).round() as usize;
    [
        Span::styled("━".repeat(filled), Style::new().fg(color)),
        Span::styled("─".repeat(width - filled), DIM),
    ]
}

/// `s` cut to `max` characters, with an ellipsis when it was longer.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max - 1).chain(['…']).collect()
    } else {
        s.to_owned()
    }
}

fn week_range(monday: NaiveDate) -> String {
    let sunday = monday + chrono::Days::new(6);
    if monday.month() == sunday.month() {
        format!("{} – {}", monday.format("%b %-d"), sunday.format("%-d"))
    } else {
        format!("{} – {}", monday.format("%b %-d"), sunday.format("%b %-d"))
    }
}

// ---- week board ---------------------------------------------------------------

/// Lines a card may wrap to before its title is cut off.
const CARD_LINES: usize = 2;

/// `Backlog | Mon … Sun`, each column a stack of wrapped cards.
fn draw_week(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(DIM);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.page = inner.height.saturating_sub(1) as usize / CARD_LINES;

    let columns = Layout::horizontal([Constraint::Fill(1); WEEK_COLUMNS]).split(inner);
    for (col, &area) in columns.iter().enumerate() {
        draw_column(frame, app, col, area);
    }
}

fn draw_column(frame: &mut Frame, app: &App, col: usize, area: Rect) {
    let focused = col == app.col;
    let block = if col == 0 {
        Block::new()
    } else {
        Block::new().borders(Borders::LEFT).border_style(DIM)
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);

    let day = app.column_day(col);
    let title = match day {
        None => "Backlog".to_owned(),
        Some(d) => d.format("%a %-d").to_string(),
    };
    let mut head_style = if day == Some(app.today) {
        Style::new().yellow().bold()
    } else {
        Style::new().bold()
    };
    if focused {
        head_style = head_style.add_modifier(Modifier::UNDERLINED);
    }
    let cards = app.column(col);
    // What's left to do on this day, by the open todos' plans.
    let left: chrono::Duration = cards
        .iter()
        .filter(|c| c.todo.status == Status::Open)
        .filter_map(|c| app.plan_left(&c.todo))
        .sum();
    let mut head_line = Line::from(Span::styled(format!(" {title}"), head_style));
    if left > chrono::Duration::zero() {
        head_line.push_span(Span::styled(format!(" {}", format_duration(left)), DIM));
    }
    frame.render_widget(head_line, head);

    let width = body.width.saturating_sub(1) as usize;
    let cursor = focused.then_some(app.cursor);
    let adding = matches!(
        app.mode,
        Mode::Input {
            kind: InputKind::NewTodo { .. },
            ..
        }
    );
    let blocks: Vec<Vec<Line>> = cards
        .iter()
        .enumerate()
        .map(|(i, card)| {
            let selected = cursor == Some(i) && !adding;
            let timer = countdown_text(app, card.todo.id);
            let left = (card.todo.status == Status::Open)
                .then(|| app.plan_left(&card.todo))
                .flatten();
            card_lines(
                card,
                width,
                selected,
                app.changed.contains(&card.todo.id),
                timer,
                left,
            )
        })
        .collect();

    // Scroll whole cards so the focused one is on screen.
    let height = body.height as usize;
    let mut first = 0;
    if let Some(cursor) = cursor.filter(|&c| c < blocks.len()) {
        // Keep a line spare for the "+N more" note below it.
        let fits = height.saturating_sub(usize::from(cursor + 1 < blocks.len()));
        while first < cursor && blocks[first..=cursor].iter().map(Vec::len).sum::<usize>() > fits {
            first += 1;
        }
    }
    let total: usize = blocks[first..].iter().map(Vec::len).sum();
    // Leave a line for the overflow note when not everything fits.
    let room = if total > height {
        height.saturating_sub(1)
    } else {
        height
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut shown = first;
    for block in blocks.into_iter().skip(first) {
        if lines.len() + block.len() > room {
            break;
        }
        lines.extend(block);
        shown += 1;
    }
    if shown < cards.len() && height > 0 {
        lines.push(Line::styled(format!(" +{} more", cards.len() - shown), DIM));
    }
    if cards.is_empty() && focused && !adding {
        lines.push(Line::styled(" o adds", DIM));
    }
    frame.render_widget(Paragraph::new(lines), body);
}

/// A card: a marker column (`•` changed, `!` high, `✓` done), then the title,
/// slip count, running timer and what's left of its plan, wrapped to at most
/// [`CARD_LINES`] lines.
fn card_lines(
    card: &Card,
    width: usize,
    selected: bool,
    changed: bool,
    timer: Option<String>,
    plan_left: Option<chrono::Duration>,
) -> Vec<Line<'static>> {
    let t = &card.todo;
    let done = t.status == Status::Done;
    let marker = if changed {
        "•".cyan().bold()
    } else if done {
        Span::styled("✓", DIM)
    } else {
        match t.priority {
            Priority::High => "!".red().bold(),
            Priority::Medium => Span::raw("·").dark_gray(),
            Priority::Low => Span::raw(" "),
        }
    };
    let mut style = if done {
        DIM.add_modifier(Modifier::CROSSED_OUT)
    } else {
        Style::new()
    };
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    let mut text = t.title.clone();
    if card.slipped > 0 {
        text.push_str(&format!(" ↻{}", card.slipped));
    }
    if let Some(left) = timer {
        text.push_str(&format!(" [{left}]"));
    }
    match plan_left {
        Some(left) if left > chrono::Duration::zero() => {
            text.push_str(&format!(" ~{}", format_duration(left)))
        }
        Some(_) => text.push_str(" ~over"),
        None => {}
    }
    wrap(&text, width.max(1), CARD_LINES)
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let lead = if i == 0 {
                marker.clone()
            } else {
                Span::raw(" ")
            };
            let pad = width.saturating_sub(line.chars().count());
            let body = if selected {
                format!("{line}{}", " ".repeat(pad))
            } else {
                line
            };
            Line::from(vec![lead, Span::styled(body, style)])
        })
        .collect()
}

/// Word-wraps `text` to `width` columns over at most `max` lines, ending the
/// last with `…` when it doesn't fit. Words longer than a line are split.
fn wrap(text: &str, width: usize, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        loop {
            let used = line.chars().count();
            let gap = usize::from(used > 0);
            if used + gap + word.len() <= width {
                if gap == 1 {
                    line.push(' ');
                }
                line.extend(&word);
                break;
            }
            if used > 0 {
                lines.push(std::mem::take(&mut line));
                continue;
            }
            // Longer than a whole line: split it.
            line.extend(&word[..width]);
            word.drain(..width);
            lines.push(std::mem::take(&mut line));
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max {
        lines.truncate(max);
        let last = &mut lines[max - 1];
        let keep = width.saturating_sub(1).min(last.chars().count());
        *last = last.chars().take(keep).collect::<String>() + "…";
    }
    lines
}

/// The focused card in full, or the line being typed for it.
fn draw_focus_strip(frame: &mut Frame, app: &App, area: Rect) {
    if let Mode::Input {
        kind: kind @ (InputKind::NewTodo { .. } | InputKind::EditTitle(_)),
        line,
    } = &app.mode
    {
        let label = match kind {
            InputKind::NewTodo { .. } => match app.column_day(app.col) {
                Some(day) => format!(" new on {}: ", day.format("%a %-d")),
                None => " new in Backlog: ".to_owned(),
            },
            _ => " title: ".to_owned(),
        };
        let mut spans = vec![Span::styled(label, DIM)];
        spans.extend(input_line(line).spans);
        frame.render_widget(Line::from(spans), area);
        return;
    }
    let Some(t) = app.current() else {
        return frame.render_widget(Line::styled(" (empty column)", DIM), area);
    };
    let mut spans = vec![
        Span::styled(format!(" #{} ", t.id), DIM),
        Span::raw(t.title.clone()).bold(),
    ];
    if let Some(p) = t.project.as_deref().filter(|p| !p.is_empty()) {
        spans.push(Span::raw(format!(" +{p}")).cyan());
    }
    if !t.tags.is_empty() {
        spans.push(Span::raw(format!(" {}", tags_text(t))).magenta());
    }
    let prio = match t.priority {
        Priority::High => "!h".red(),
        Priority::Medium => "!m".yellow(),
        Priority::Low => "!l".dark_gray(),
    };
    spans.extend([" ".into(), prio]);
    if let Some(day) = t.scheduled {
        spans.push(Span::styled(
            format!(" · planned {}", day.format("%a %b %-d")),
            DIM,
        ));
    }
    let plan = plan_text(app, t);
    if plan.width() > 0 {
        spans.extend([Span::styled(" · ", DIM), plan]);
    }
    frame.render_widget(Line::from(spans), area);
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
            InputKind::TimerLength(_) => Some("focus for (minutes, or 50m / 1h30m): "),
            InputKind::Plan(_) => None,
            InputKind::AddSession(_) => Some("add time worked, ending now (34, 1h30m): "),
            _ => None,
        };
        let length_prompt = match kind {
            InputKind::Plan(id) => {
                let now = app
                    .all_todos()
                    .iter()
                    .find(|t| t.id == *id)
                    .and_then(Todo::plan)
                    .map_or("none".into(), format_duration);
                Some(format!("plan (now {now}; empty clears): "))
            }
            InputKind::SessionLength { id, index } => app
                .all_todos()
                .iter()
                .find(|t| t.id == *id)
                .and_then(|t| t.sessions.get(*index))
                .map(|s| format!("length (now {}): ", format_duration(s.duration()))),
            _ => None,
        };
        if let Some(prompt) = length_prompt.as_deref().or(prompt) {
            let mut spans = vec![Span::raw(prompt.to_owned())];
            spans.extend(input_line(line).spans);
            frame.render_widget(Line::from(spans), area);
            return;
        }
    }

    let (label, color) = match &app.mode {
        Mode::Input { .. } => (" INSERT ", Color::Green),
        Mode::Picker { .. } => (" PROJECT ", Color::Cyan),
        Mode::Log { .. } => (" LOG ", Color::Magenta),
        _ if app.view == View::Week => (" WEEK ", Color::Yellow),
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
            "Enter/Esc save · !h !m !l priority · +project · #tag · ~2h plan",
            DIM,
        )),
        (None, Mode::Log { .. }) => spans.push(Span::styled(
            "j/k move · Enter go to todo · r revert · a others only · q back",
            DIM,
        )),
        (None, _) if app.view == View::Week => spans.push(Span::styled(
            "h/l day · H/L shove · gt today · gb backlog · gw pick day · C-n/C-p week · W list",
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

fn draw_log_header(frame: &mut Frame, app: &App, area: Rect) {
    let Mode::Log {
        entries,
        others_only,
        ..
    } = &app.mode
    else {
        return;
    };
    let scope = if *others_only {
        "others only"
    } else {
        "everyone"
    };
    let left = Line::from(vec![
        " agentodo".bold(),
        Span::styled(" — ", DIM),
        "change log".magenta(),
        Span::styled(format!(" · {scope}"), DIM),
    ]);
    let right = Line::from(format!(" [{} changes] ", entries.len())).right_aligned();
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right.width() as u16),
    ])
    .areas(area);
    frame.render_widget(left, left_area);
    frame.render_widget(right, right_area);
}

fn draw_log(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(DIM);
    app.page = block.inner(area).height as usize;
    let Mode::Log {
        entries, selected, ..
    } = &app.mode
    else {
        return;
    };

    if entries.is_empty() {
        let hint = Paragraph::new(Span::styled("No changes logged yet", DIM)).block(block);
        frame.render_widget(hint, area);
        return;
    }

    let actor_width = entries
        .iter()
        .map(|e| actor_label(e).chars().count())
        .max()
        .unwrap_or(0)
        .min(20) as u16;
    let id_width = entries
        .iter()
        .map(|e| e.id.to_string().len() + 1)
        .max()
        .unwrap_or(2) as u16;

    let rows = entries.iter().map(|e| {
        let kind = match e.kind() {
            EntryKind::Created => "created".green(),
            EntryKind::Updated => "updated".yellow(),
            EntryKind::Deleted => "deleted".red(),
        };
        let actor = match e.actor.source {
            Source::Tui => Span::styled(actor_label(e), DIM),
            _ => Span::raw(actor_label(e)).cyan(),
        };
        Row::new(vec![
            Cell::from(Span::styled(
                e.at.with_timezone(&Local).format("%m-%d %H:%M").to_string(),
                DIM,
            )),
            Cell::from(actor),
            Cell::from(kind),
            Cell::from(Line::from(format!("#{}", e.id)).right_aligned()).style(DIM),
            Cell::from(e.title().to_owned()),
            Cell::from(Span::styled(diff_text(e), DIM)),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(11),
            Constraint::Length(actor_width),
            Constraint::Length(7),
            Constraint::Length(id_width),
            Constraint::Fill(2),
            Constraint::Fill(3),
        ],
    )
    .block(block)
    .column_spacing(1)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ")
    .highlight_spacing(HighlightSpacing::Always);
    let mut state = TableState::default().with_selected(Some(*selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// `mcp:claude-code`, or just the source when there is no client name.
fn actor_label(e: &LogEntry) -> String {
    let source = match e.actor.source {
        Source::Cli => "cli",
        Source::Tui => "tui",
        Source::Mcp => "mcp",
        Source::Unknown => "?",
    };
    match &e.actor.client {
        Some(client) => format!("{source}:{client}"),
        None => source.to_owned(),
    }
}

/// `status open→done, priority medium→high`; notes only say they changed.
fn diff_text(e: &LogEntry) -> String {
    e.diff()
        .iter()
        .map(|c| match c.field {
            "notes" => "notes edited".to_owned(),
            field => {
                let show = |v: &str| {
                    if v.is_empty() {
                        "—".to_owned()
                    } else {
                        v.to_owned()
                    }
                };
                format!("{field} {}→{}", show(&c.old), show(&c.new))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
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

struct HelpSection {
    title: &'static str,
    note: &'static str,
    keys: &'static [(&'static str, &'static str)],
}

const VIM_KEYS: HelpSection = HelpSection {
    title: "vim",
    note: "works as in vim",
    keys: &[
        ("j k ↓ ↑", "move (count: 5j)"),
        ("gg G 5G", "top / bottom / row 5"),
        ("C-d C-u", "half page down / up"),
        ("o O", "add below / above"),
        ("i  cc", "edit / retype title"),
        ("dd 3dd", "delete (with count)"),
        ("u  C-r", "undo / redo"),
        ("/  n N", "search / next / prev"),
        (":  :5", "command line / row 5"),
        (":q  ZZ", "quit"),
    ],
};

const AGENTODO_KEYS: HelpSection = HelpSection {
    title: "agentodo",
    note: "todo-specific",
    keys: &[
        ("x", "toggle done"),
        ("+ -", "raise / lower priority"),
        ("gp", "set project"),
        ("e", "edit notes in $EDITOR"),
        ("T  :stop", "focus timer: start / stop"),
        ("P  :pause :resume", "pause / resume the timer"),
        ("S  gT", "stop timer / jump to its todo"),
        ("t", "plan time (~2h when adding)"),
        ("Enter", "details · fix time (c dd a)"),
        ("p", "filter by project"),
        (".", "show / hide done"),
        ("s", "sort priority / created"),
        ("W  :week", "week board (:list back)"),
        (":project [p]", "filter; no name = all"),
        (":sort p|c", "sort priority / created"),
        (":hide :show", "hide / show done"),
        ("]c [c", "next / prev changed (•)"),
        (":log", "change log (r reverts)"),
        (":seen", "clear • markers"),
        ("q", "quit"),
        ("?", "this help"),
    ],
};

const WEEK_KEYS: HelpSection = HelpSection {
    title: "week",
    note: "planning board",
    keys: &[
        ("h l", "previous / next column"),
        ("H L 3L", "shove a day earlier / later"),
        ("gt", "schedule for today"),
        ("gb", "back to Backlog"),
        ("gw m..u b", "send to weekday / Backlog"),
        ("C-n C-p", "next / previous week"),
        ("o O", "add on this column's day"),
        ("↻3", "slipped 3 days, shown today"),
    ],
};

const QUICK_ADD_KEYS: HelpSection = HelpSection {
    title: "quick-add",
    note: "while typing a title",
    keys: &[
        ("!h !m !l", "high / medium / low"),
        ("+name", "set project"),
        ("~2h", "plan time for it"),
        ("#name", "add a tag"),
        ("Enter Esc", "save"),
    ],
};

fn section_lines(section: &HelpSection) -> Vec<Line<'static>> {
    let key_width = section
        .keys
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        .max(9);
    let mut lines = vec![Line::from(vec![
        section.title.bold().cyan(),
        Span::styled(format!("  {}", section.note), DIM),
    ])];
    lines.extend(section.keys.iter().map(|(keys, what)| {
        Line::from(vec![
            format!(" {keys:<key_width$}  ").bold().yellow(),
            Span::raw(*what),
        ])
    }));
    lines
}

/// Vim keys (plus the quick-add syntax used with `o`) on the left and
/// agentodo's own keys on the right, or stacked when the terminal is too
/// narrow for both.
fn draw_help(frame: &mut Frame) {
    let mut vim = section_lines(&VIM_KEYS);
    vim.push(Line::default());
    vim.extend(section_lines(&QUICK_ADD_KEYS));
    let mut ours = section_lines(&AGENTODO_KEYS);
    ours.push(Line::default());
    ours.extend(section_lines(&WEEK_KEYS));

    let width = |lines: &[Line]| lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 1;
    let (left, right) = (width(&vim), width(&ours));
    // Indent + left column, separator + space, right column, border.
    let wide = 1 + left + 2 + right + 2;
    let indent = Block::new().padding(Padding::left(1));

    if frame.area().width >= wide {
        let height = vim.len().max(ours.len()) as u16 + 2;
        let inner = popup(frame, wide, height, "keys");
        let [l, sep, r] = Layout::horizontal([
            Constraint::Length(left + 1),
            Constraint::Length(2),
            Constraint::Fill(1),
        ])
        .areas(inner);
        frame.render_widget(Paragraph::new(vim).block(indent), l);
        frame.render_widget(Block::new().borders(Borders::LEFT).border_style(DIM), sep);
        frame.render_widget(Paragraph::new(ours), r);
    } else {
        let mut lines = vim;
        lines.push(Line::default());
        lines.extend(ours);
        let inner = popup(frame, left.max(right) + 3, lines.len() as u16 + 2, "keys");
        frame.render_widget(Paragraph::new(lines).block(indent), inner);
    }
}

/// Sessions listed at once in the detail view; the rest scroll into view.
const DETAIL_SESSIONS: usize = 10;

fn draw_detail(frame: &mut Frame, app: &App, id: u64, selected: usize) {
    let Some(t) = app.all_todos().iter().find(|t| t.id == id) else {
        return;
    };
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
    if let Some(clock) = countdown(app, t.id) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>10}  ", "timer"), DIM),
            clock,
        ]));
    }
    if let Some(plan) = t.plan() {
        let spent = t.time_spent() + app.running_on(t.id);
        let status = if spent > plan {
            Span::raw(format!("{} over", format_duration(spent - plan))).yellow()
        } else {
            Span::raw(format!("{} left", format_duration(plan - spent)))
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:>10}  ", "planned"), DIM),
            Span::raw(format!(
                "{} · {} spent · ",
                format_duration(plan),
                format_duration(spent)
            )),
            status,
        ]));
    }
    lines.push(field("time", time_summary(t)));
    lines.push(Line::default());
    lines.extend(session_lines(t, selected));
    lines.push(Line::default());
    match t.notes.as_deref() {
        Some(notes) => lines.extend(Text::from(notes).lines),
        None => lines.push(Line::styled("no notes — press e to write some", DIM)),
    }
    lines.push(Line::default());
    lines.push(Line::styled(
        "j/k session · c change · dd delete · a add · u undo · e notes · q close",
        DIM,
    ));

    let area = frame.area();
    let width = area.width.saturating_sub(8).min(80);
    let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let inner = popup(frame, width, height, "todo");
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// `1h12m over 3 sessions`, or `—`.
fn time_summary(t: &Todo) -> String {
    match t.sessions.len() {
        0 => "—".into(),
        n => format!(
            "{} over {n} session{}",
            format_duration(t.time_spent()),
            if n == 1 { "" } else { "s" }
        ),
    }
}

/// Sessions newest first, [`DETAIL_SESSIONS`] at a time around the
/// selected one, overtime called out so a forgotten timer stands out.
fn session_lines(t: &Todo, selected: usize) -> Vec<Line<'static>> {
    let total = t.sessions.len();
    if total == 0 {
        return vec![Line::styled(
            "  no time recorded — T starts a timer, a adds time",
            DIM,
        )];
    }
    let first = selected.saturating_sub(DETAIL_SESSIONS - 1);
    let mut lines = vec![Line::from("  sessions".bold())];
    if first > 0 {
        lines.push(Line::styled(format!("    ↑ {first} newer"), DIM));
    }
    for (i, s) in t
        .sessions
        .iter()
        .rev()
        .enumerate()
        .skip(first)
        .take(DETAIL_SESSIONS)
    {
        let (start, end) = (s.start.with_timezone(&Local), s.end.with_timezone(&Local));
        let mut spans = vec![
            Span::raw(if i == selected { "  ▶ " } else { "    " }),
            Span::raw(format!(
                "{}  {}–{}  {:>6}",
                start.format("%a %-d %b"),
                start.format("%H:%M"),
                end.format("%H:%M"),
                format_duration(s.duration())
            )),
        ];
        if let Some(over) = s.overtime() {
            spans.push(Span::raw(format!("  +{} over", format_duration(over))).yellow());
        }
        if s.paused_secs > 0 {
            spans.push(Span::styled(
                format!("  ({} paused)", format_duration(s.paused())),
                DIM,
            ));
        }
        let line = Line::from(spans);
        lines.push(if i == selected {
            line.add_modifier(Modifier::REVERSED)
        } else {
            line
        });
    }
    let shown_to = (first + DETAIL_SESSIONS).min(total);
    if shown_to < total {
        lines.push(Line::styled(
            format!("    +{} earlier", total - shown_to),
            DIM,
        ));
    }
    lines
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
        assert!(row_text(&buf, ship).starts_with("▶   ●   2 ship release"));
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
        let buf = render(&mut app, 70, 40);
        find_row(&buf, " keys ");
        find_row(&buf, "toggle done");
    }

    #[test]
    fn help_puts_vim_and_agentodo_keys_side_by_side_when_wide() {
        let ts = TempStore::new();
        let mut app = ts.app();
        press(&mut app, "?");

        let buf = render(&mut app, 100, 24);
        let y = find_row(&buf, "works as in vim");
        assert!(row_text(&buf, y).contains("todo-specific"));

        let buf = render(&mut app, 50, 40);
        assert!(find_row(&buf, "todo-specific") > find_row(&buf, "works as in vim"));
        find_row(&buf, "while typing a title");
    }

    #[test]
    fn change_marker_and_log_view_render() {
        let ts = TempStore::new();
        ts.seed(&["mine"]);
        let seen = chrono::Utc::now();
        ts.as_agent(|s| {
            let id = s.add(agentodo_core::NewTodo {
                title: "from agent".into(),
                ..Default::default()
            });
            s.set_status(id.id, agentodo_core::Status::Done).unwrap();
        });
        let mut app = ts.app_seen(seen);

        let buf = render(&mut app, 70, 8);
        let y = find_row(&buf, "from agent");
        assert_eq!(buf[(2, y)].symbol(), "•");
        assert_ne!(buf[(2, find_row(&buf, "mine"))].symbol(), "•");

        press(&mut app, ":log<CR>");
        let buf = render(&mut app, 100, 8);
        assert!(row_text(&buf, 0).contains("change log"));
        let y = find_row(&buf, "status open→done");
        assert!(row_text(&buf, y).contains("mcp:bot"));
        assert!(row_text(&buf, y).contains("updated"));
        assert!(row_text(&buf, 7).contains("LOG"));
    }

    #[test]
    #[ignore = "prints a frame for eyeballing: cargo test -p agentodo-tui -- --ignored --nocapture"]
    fn print_frame() {
        // Help overlay, wide and narrow.
        let ts_help = TempStore::new();
        let mut help = ts_help.app();
        press(&mut help, "?");
        for (w, h) in [(90, 20), (48, 36)] {
            let buf = render(&mut help, w, h);
            for y in 0..buf.area.height {
                println!("{}", row_text(&buf, y));
            }
        }

        let ts = TempStore::new();
        ts.seed(&[
            "buy milk !l +home #errand",
            "write docs",
            "ship release !h +work #urgent #release",
            "answer email +work",
        ]);
        ts.as_agent(|s| {
            s.start_timer(4, chrono::Duration::minutes(25), chrono::Utc::now())
                .unwrap();
        });
        let mut app = ts.app();
        press(&mut app, "Gx/re<CR>");
        let buf = render(&mut app, 72, 10);
        for y in 0..buf.area.height {
            println!("{}", row_text(&buf, y));
        }
    }

    #[test]
    fn wrap_breaks_words_and_ellipsizes() {
        assert_eq!(
            wrap("bulk add shopping in flat rota", 10, 2),
            ["bulk add", "shopping…"]
        );
        assert_eq!(wrap("IaC project", 10, 2), ["IaC", "project"]);
        assert_eq!(wrap("supercalifragilistic", 8, 2), ["supercal", "ifragil…"]);
        assert_eq!(wrap("short", 10, 2), ["short"]);
    }

    fn friday() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()
    }

    #[test]
    fn week_board_renders_columns_cards_and_focus_strip() {
        let ts = TempStore::new();
        ts.seed(&["someday thing", "refactor store !h +agentodo #core"]);
        let mut store = ts.store();
        store
            .update(
                2,
                agentodo_core::TodoPatch {
                    scheduled: Some(Some(chrono::NaiveDate::from_ymd_opt(2026, 9, 23).unwrap())),
                    ..Default::default()
                },
            )
            .unwrap();
        store.save().unwrap();
        let mut app = ts.app();
        app.set_clock(friday);
        press(&mut app, ":week<CR>");

        let buf = render(&mut app, 120, 12);
        assert!(row_text(&buf, 0).contains("week of Sep 21 – 27"));
        let heads = row_text(&buf, 2);
        for day in ["Backlog", "Mon 21", "Wed 23", "Fri 25", "Sun 27"] {
            assert!(heads.contains(day), "{day} in {heads:?}");
        }
        // The Wednesday card slipped to Friday, today's column.
        let y = find_row(&buf, "refactor");
        let x = row_text(&buf, y).find("refactor").unwrap();
        let fri = heads.find("Fri 25").unwrap();
        assert!(x >= fri, "slipped card sits under today");
        assert!(find_row(&buf, "↻2") >= y);
        assert!(row_text(&buf, y).contains("!refactor"));

        let strip = row_text(&buf, 10);
        assert!(
            strip.contains("#2 refactor store +agentodo #core !h"),
            "{strip:?}"
        );
        assert!(row_text(&buf, 11).contains("WEEK"));

        press(&mut app, "5h");
        let buf = render(&mut app, 120, 12);
        assert!(row_text(&buf, 10).contains("#1 someday thing"));
        press(&mut app, "ohello");
        let buf = render(&mut app, 120, 12);
        assert!(row_text(&buf, 10).contains("new in Backlog: hello"));
    }

    #[test]
    fn full_columns_scroll_to_the_cursor_and_count_the_rest() {
        let ts = TempStore::new();
        let titles: Vec<String> = (1..=9).map(|i| format!("task {i}")).collect();
        ts.seed(&titles.iter().map(String::as_str).collect::<Vec<_>>());
        let mut app = ts.app();
        app.set_clock(friday);
        press(&mut app, ":week<CR>");
        press(&mut app, "5h");
        // Body is 4 lines: 3 cards plus the overflow note.
        let buf = render(&mut app, 120, 10);
        assert!(find_row(&buf, "+6 more") > 0);
        press(&mut app, "G");
        let buf = render(&mut app, 120, 10);
        let last = &app.current().unwrap().title.clone();
        find_row(&buf, last);
    }

    fn nine() -> chrono::DateTime<chrono::Utc> {
        "2026-10-05T09:00:00Z".parse().unwrap()
    }

    fn nine_ish() -> chrono::DateTime<chrono::Utc> {
        nine() + chrono::Duration::seconds(6 * 60 + 18)
    }

    #[test]
    fn running_timer_counts_down_in_player_row_card_and_detail() {
        let ts = TempStore::new();
        ts.seed(&["buy milk", "refactor store +work"]);
        ts.as_agent(|s| {
            s.start_timer(2, chrono::Duration::minutes(25), nine())
                .unwrap();
        });
        let mut app = ts.app();
        app.set_now(nine_ish);

        // A short screen folds the player into one line above the footer.
        let buf = render(&mut app, 90, 8);
        let header = row_text(&buf, 0);
        assert!(!header.contains("18:42"), "{header}");
        assert!(header.trim_end().ends_with("[2 open / 0 done]"));
        let player = row_text(&buf, 6);
        assert!(
            player.starts_with(" ▶ 18:42  #2 refactor store ━"),
            "{player}"
        );
        assert!(player.trim_end().ends_with("S stop · gT jump"), "{player}");
        assert_eq!(buf[(1, 6)].bg, Color::Green);

        let y = (1..6)
            .find(|&y| row_text(&buf, y).contains("refactor store"))
            .unwrap();
        assert!(row_text(&buf, y).contains("18:42"));
        assert_eq!(buf[(6, y)].bg, Color::Green, "the timed id is badged");
        let milk = find_row(&buf, "buy milk");
        assert!(!row_text(&buf, milk).contains("18:42"));
        assert_ne!(buf[(6, milk)].bg, Color::Green);

        // A tall one docks the full card, centred.
        let buf = render(&mut app, 90, 24);
        let top = find_row(&buf, "╭ focus ");
        assert_eq!(row_text(&buf, top).find('╭'), Some(9));
        let title = row_text(&buf, top + 1);
        assert!(title.contains("│ ▶  #2 refactor store"), "{title}");
        assert!(title.contains("work · ends "), "{title}");
        let bar = row_text(&buf, top + 2);
        assert!(bar.contains("│ 06:18 ━━━━━"), "{bar}");
        assert!(bar.contains("─ -18:42 │"), "{bar}");
        assert!(row_text(&buf, top + 3).contains("S stop · gT jump ╯"));
        assert_eq!(top + 4, 23, "docked right above the footer");

        press(&mut app, "W");
        let buf = render(&mut app, 120, 10);
        find_row(&buf, "[18:42]");

        press(&mut app, "W");
        press(&mut app, "/refactor<CR><CR>");
        let buf = render(&mut app, 90, 30);
        assert!(row_text(&buf, find_row(&buf, "timer")).contains("18:42"));
        find_row(&buf, "no time recorded");
    }

    fn much_later() -> chrono::DateTime<chrono::Utc> {
        nine() + chrono::Duration::hours(30)
    }

    #[test]
    fn a_paused_timer_greys_the_player_and_says_how_to_resume() {
        let ts = TempStore::new();
        ts.seed(&["refactor store +work"]);
        ts.as_agent(|s| {
            s.start_timer(1, chrono::Duration::minutes(25), nine())
                .unwrap();
            s.pause_timer(nine_ish());
        });
        let mut app = ts.app();
        app.set_now(much_later);

        let buf = render(&mut app, 90, 8);
        let player = row_text(&buf, 6);
        assert!(
            player.starts_with(" ⏸ 18:42  #1 refactor store"),
            "{player}"
        );
        assert_eq!(buf[(1, 6)].bg, Color::DarkGray);

        let buf = render(&mut app, 90, 24);
        let top = find_row(&buf, "╭ focus ");
        let title = row_text(&buf, top + 1);
        assert!(title.contains("│ ⏸  #1 refactor store"), "{title}");
        assert!(title.contains("work · paused · P resumes"), "{title}");
        assert!(row_text(&buf, top + 2).contains("│ 06:18 ━"));

        // Left running instead, it pauses itself and says so.
        ts.as_agent(|s| {
            s.set_max_overtime(Some(chrono::Duration::hours(1)));
            s.start_timer(1, chrono::Duration::minutes(25), nine())
                .unwrap();
        });
        app.refresh_if_changed();
        let buf = render(&mut app, 90, 24);
        let top = find_row(&buf, "╭ focus ");
        let title = row_text(&buf, top + 1);
        assert!(title.contains("paused itself · P resumes"), "{title}");
        assert!(row_text(&buf, top + 2).contains("+1:00:00 │"));
    }

    fn overtime_now() -> chrono::DateTime<chrono::Utc> {
        nine() + chrono::Duration::seconds(25 * 60 + 72)
    }

    #[test]
    fn overtime_counts_up_and_sessions_list_newest_first() {
        let ts = TempStore::new();
        ts.seed(&["refactor store"]);
        ts.as_agent(|s| {
            s.set_max_overtime(None);
            for _ in 0..12 {
                s.add_session(1, chrono::Duration::minutes(5), nine())
                    .unwrap();
            }
            s.start_timer(
                1,
                chrono::Duration::minutes(25),
                nine() - chrono::Duration::hours(10),
            )
            .unwrap();
            s.stop_timer(nine() - chrono::Duration::minutes(35))
                .unwrap();
            s.start_timer(1, chrono::Duration::minutes(25), nine())
                .unwrap();
        });
        let mut app = ts.app();
        app.set_now(overtime_now);

        let buf = render(&mut app, 90, 8);
        assert!(row_text(&buf, 6).starts_with(" ▶ +01:12  #1"));
        assert_eq!(buf[(1, 6)].bg, Color::Yellow);

        let buf = render(&mut app, 90, 24);
        let top = find_row(&buf, "╭ focus ");
        assert_eq!(buf[(9, top)].fg, Color::Yellow);
        assert!(row_text(&buf, top + 1).contains("+01:12 over │"));
        let bar = row_text(&buf, top + 2);
        assert!(bar.contains("│ 26:12 ━━━━"), "{bar}");
        assert!(bar.contains("━ +01:12 │"), "a full bar: {bar}");
        assert!(!bar.contains('─'), "{bar}");

        // Far enough over and it looks forgotten.
        app.set_now(|| nine() + chrono::Duration::minutes(25 + 15));
        let buf = render(&mut app, 90, 24);
        assert_eq!(buf[(9, find_row(&buf, "╭ focus "))].fg, Color::Red);
        app.set_now(overtime_now);

        press(&mut app, "<CR>");
        let buf = render(&mut app, 90, 40);
        assert!(row_text(&buf, find_row(&buf, "time ")).contains("10h25m over 13 sessions"));
        let newest = find_row(&buf, "  ▶ ");
        assert!(row_text(&buf, newest).contains("9h25m  +9h over"));
        assert!(row_text(&buf, newest + 1).contains("5m"));
        find_row(&buf, "+3 earlier");

        press(&mut app, "c");
        let buf = render(&mut app, 90, 40);
        assert!(row_text(&buf, 39).starts_with("length (now 9h25m): "));
        find_row(&buf, "sessions");
    }

    #[test]
    fn plans_show_in_the_list_week_board_and_detail() {
        let ts = TempStore::new();
        ts.seed(&["refactor store ~2h", "write docs ~30m", "buy milk"]);
        ts.as_agent(|s| {
            s.add_session(1, chrono::Duration::minutes(45), nine())
                .unwrap();
            s.add_session(2, chrono::Duration::minutes(50), nine())
                .unwrap();
            for id in [1, 2, 3] {
                s.update(
                    id,
                    agentodo_core::TodoPatch {
                        scheduled: Some(Some(friday())),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
        });
        let mut app = ts.app();
        app.set_clock(friday);

        let buf = render(&mut app, 90, 8);
        let row = |needle| row_text(&buf, find_row(&buf, needle));
        assert!(row("refactor store").contains("45m/2h"));
        assert!(row("write docs").contains("50m/30m"));
        assert!(!row("buy milk").contains('/'));
        let over = find_row(&buf, "write docs");
        let x = row_text(&buf, over).find("50m/30m").unwrap();
        let x = row_text(&buf, over)[..x].chars().count() as u16;
        assert_eq!(buf[(x, over)].fg, Color::Yellow);

        press(&mut app, "W");
        let buf = render(&mut app, 160, 12);
        assert!(row_text(&buf, find_row(&buf, "Fri 25")).contains("Fri 25 1h15m"));
        find_row(&buf, "~1h15m");
        find_row(&buf, "~over");

        press(&mut app, "W/refactor<CR><CR>");
        let buf = render(&mut app, 90, 30);
        assert!(row_text(&buf, find_row(&buf, "planned")).contains("2h · 45m spent · 1h15m left"));
    }
}
