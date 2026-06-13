use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{App, EditState, EntryStatus, Mode, Row};
use crate::matching::{Destination, SortEntry};

// ── Entry point ───────────────────────────────────────────────────────────────

/// What the review session left unsorted, for the closing summary.
pub struct RunSummary {
    pub skipped: usize,
    pub pending: usize,
}

pub fn run(entries: Vec<SortEntry>, dest_root: PathBuf) -> Result<RunSummary> {
    let mut app = App::new(entries, dest_root);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let run_result = event_loop(&mut terminal, &mut app);

    // Restore terminal unconditionally — even if we errored
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    run_result?;

    if let Some(msg) = &app.message {
        println!("{}", msg);
    }

    Ok(RunSummary {
        skipped: app.skipped_count(),
        pending: app.pending_count(),
    })
}

// ── Event loop ────────────────────────────────────────────────────────────────

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut list_state = ListState::default();

    loop {
        list_state.select(Some(app.selected));
        terminal.draw(|f| draw(f, app, &mut list_state))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        if let Event::Key(key) = event::read()? {
            // Extract a mode tag without borrowing app through the match body.
            // This lets us pass &mut app to the handlers below.
            let f = match &app.mode {
                Mode::Normal     => handle_normal,
                Mode::Editing(_) => handle_editing,
                Mode::Confirming => handle_confirming,
                Mode::Done       => break,
            };

            f(app, key.code)
        }

        if app.mode == Mode::Done {
            break;
        }
    }

    Ok(())
}

// ── Key handlers ──────────────────────────────────────────────────────────────

fn handle_normal(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Up   | KeyCode::Char('k') => app.move_up(),
        KeyCode::Down | KeyCode::Char('j') => app.move_down(),
        KeyCode::PageUp                    => app.page_up(10),
        KeyCode::PageDown                  => app.page_down(10),
        KeyCode::Char('g')                 => app.jump_top(),
        KeyCode::Char('G')                 => app.jump_bottom(),
        KeyCode::Right | KeyCode::Char('l') => app.expand_selected(),
        KeyCode::Left  | KeyCode::Char('h') => app.collapse_selected(),
        KeyCode::Char(' ')                 => app.toggle_selected(),
        KeyCode::Enter | KeyCode::Char('a') => app.approve_selected(),
        KeyCode::Char('A')                 => app.approve_all_ready(),
        KeyCode::Char('s')                 => app.skip_selected(),
        KeyCode::Char('e')                 => app.start_editing(),
        KeyCode::Char('c') | KeyCode::Char('w') => app.mode = Mode::Confirming,
        KeyCode::Char('q') | KeyCode::Esc  => app.mode = Mode::Done,
        _ => {}
    }
}

fn handle_editing(app: &mut App, key: KeyCode) {
    // Enter/Esc change the app's mode, so they need &mut App; everything else is
    // pure text editing, scoped to a borrow of the EditState.
    match key {
        KeyCode::Enter => app.confirm_edit(),
        KeyCode::Esc   => app.cancel_edit(),
        other => {
            let Some(s) = app.editing_mut() else { return };
            match other {
                KeyCode::Char(c)   => s.insert(c),
                KeyCode::Backspace => s.backspace(),
                KeyCode::Delete    => s.delete(),
                KeyCode::Left      => s.cursor_left(),
                KeyCode::Right     => s.cursor_right(),
                KeyCode::Home      => s.cursor_home(),
                KeyCode::End       => s.cursor_end(),
                _ => {}
            }
        }
    }
}

fn handle_confirming(app: &mut App, key: KeyCode) {
    match key {
        KeyCode::Char('y') | KeyCode::Enter => {
            match app.commit() {
                Ok(actions) => {
                    app.message = Some(format!("done. moved {} file(s).", actions.len()));
                    app.mode = Mode::Done;
                }
                Err(e) => {
                    app.message = Some(format!("error: {}", e));
                    app.mode = Mode::Normal;
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Esc => app.mode = Mode::Normal,
        _ => {}
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App, list_state: &mut ListState) {
    let area = f.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // file list — takes all remaining space
            Constraint::Length(7), // detail panel
            Constraint::Length(3), // status bar
        ])
        .split(area);

    draw_list(f, app, chunks[0], list_state);
    draw_detail(f, app, chunks[1]);
    draw_statusbar(f, app, chunks[2]);

    // Overlays are drawn last so they appear on top
    if let Mode::Editing(ref state) = app.mode {
        draw_edit_overlay(f, state, area);
    }
    if app.mode == Mode::Confirming {
        draw_confirm_overlay(f, app, area);
    }
}

fn draw_list(f: &mut Frame, app: &App, area: Rect, list_state: &mut ListState) {
    let width = area.width as usize;

    // Build one ListItem per visible row, in the exact same order as
    // `visible_rows` — that order is what `selected` (and the highlight) indexes.
    let items: Vec<ListItem> = app
        .visible_rows()
        .iter()
        .map(|row| match *row {
            Row::Header(gi) => header_item(app, gi, width),
            Row::Single(ei) => file_item(app, ei, width, false),
            Row::Child(_, ei) => file_item(app, ei, width, true),
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Screenshots "))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, list_state);
}

/// A collapsible header line for a multi-file series.
fn header_item(app: &App, gi: usize, width: usize) -> ListItem<'static> {
    let group = &app.groups[gi];
    let st = app.group_status(gi);

    let (status_char, status_color) = if st.unresolved > 0 {
        ("!", Color::Red)
    } else if st.approved == st.total {
        ("✓", Color::Green)
    } else if st.skipped == st.total {
        ("s", Color::DarkGray)
    } else {
        ("~", Color::Yellow)
    };

    let arrow = if group.expanded { "▾" } else { "▸" };
    let (dest_str, dest_color) = group_destination(app, gi);
    let label = truncate_chars(&group.label, width.saturating_sub(44));

    ListItem::new(Line::from(vec![
        Span::styled(format!("[{}] ", status_char), Style::default().fg(status_color)),
        Span::styled(format!("{} ", arrow), Style::default().fg(Color::Cyan)),
        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  ({})", st.total), Style::default().fg(Color::DarkGray)),
        Span::raw(" → "),
        Span::styled(dest_str, Style::default().fg(dest_color)),
        Span::styled(
            format!("  {}/{} ✓", st.approved, st.total),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
}

/// A single file row — used both for one-file series and for children under an
/// expanded header. Children are indented and drop the destination (the header
/// already shows it), keeping just the filename and episode.
fn file_item(app: &App, ei: usize, width: usize, indented: bool) -> ListItem<'static> {
    let entry = &app.entries[ei];

    let (status_char, mut status_color) = match entry.status {
        EntryStatus::Approved => ("✓", Color::Green),
        EntryStatus::Pending  => ("~", Color::Yellow),
        EntryStatus::Skipped  => ("s", Color::DarkGray),
    };
    if entry.status == EntryStatus::Pending
        && matches!(entry.effective_destination(), Destination::Unresolved)
    {
        status_color = Color::Red;
    }

    let filename = base_name(&entry.sort.file.path);

    let indent = if indented { "    " } else { "" };
    let reserve = if indented { 16 } else { 40 };
    let filename_display = truncate_chars(&filename, width.saturating_sub(reserve));

    let mut spans = vec![
        Span::styled(format!("[{}] ", status_char), Style::default().fg(status_color)),
        Span::raw(format!("{}{}", indent, filename_display)),
    ];

    if indented {
        if let Some(ep) = &entry.sort.file.episode {
            spans.push(Span::styled(format!("  ep.{}", ep), Style::default().fg(Color::DarkGray)));
        }
    } else {
        let (dest_str, dest_color) = dest_display(entry.effective_destination());
        let source_label = entry.sort.source.as_ref().map(|s| s.label()).unwrap_or("?");
        spans.push(Span::raw(" → "));
        spans.push(Span::styled(dest_str, Style::default().fg(dest_color)));
        spans.push(Span::styled(format!(" [{}]", source_label), Style::default().fg(Color::DarkGray)));
    }

    ListItem::new(Line::from(spans))
}

/// The destination a whole group resolves to (its files all share one).
fn group_destination(app: &App, gi: usize) -> (String, Color) {
    let ei = app.groups[gi].entry_indices[0];
    dest_display(app.entries[ei].effective_destination())
}

/// Char-safe truncation with an ellipsis (filenames can contain Japanese etc.,
/// so we must count chars, not bytes).
fn truncate_chars(s: &str, max: usize) -> String {
    if max > 1 && s.chars().count() > max {
        let t: String = s.chars().take(max - 1).collect();
        format!("{}…", t)
    } else {
        s.to_string()
    }
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let lines = match app.selected_row() {
        Some(Row::Header(gi)) => group_detail_lines(app, gi),
        Some(Row::Single(ei)) | Some(Row::Child(_, ei)) => file_detail_lines(app, ei),
        None => vec![Line::raw("")],
    };

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Details "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Detail lines for a selected group header: series, count, status breakdown,
/// destination.
fn group_detail_lines(app: &App, gi: usize) -> Vec<Line<'static>> {
    let group = &app.groups[gi];
    let st = app.group_status(gi);
    let dim = Style::default().fg(Color::DarkGray);
    let (dest_str, dest_color) = group_destination(app, gi);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("series:  ", dim),
            Span::styled(group.label.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::from(vec![
            Span::styled("files:   ", dim),
            Span::raw(st.total.to_string()),
        ]),
    ];

    let first = group.entry_indices[0];
    if let Some(al) = &app.entries[first].sort.anilist {
        lines.push(Line::from(vec![
            Span::styled("anilist: ", dim),
            Span::styled(
                al.romaji.clone().unwrap_or_else(|| "?".to_string()),
                Style::default().fg(Color::Cyan),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("status:  ", dim),
        Span::styled(format!("✓ {}   ", st.approved), Style::default().fg(Color::Green)),
        Span::styled(format!("~ {}   ", st.pending), Style::default().fg(Color::Yellow)),
        Span::styled(format!("s {}", st.skipped), dim),
    ]));

    lines.push(Line::from(Span::styled(
        format!("→  {}", dest_str),
        Style::default().fg(dest_color),
    )));

    lines
}

/// Detail lines for a selected file row.
fn file_detail_lines(app: &App, ei: usize) -> Vec<Line<'static>> {
    let entry = &app.entries[ei];
    let file = &entry.sort.file;
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines = vec![
        Line::from(vec![
            Span::styled("file:    ", dim),
            Span::raw(base_name(&file.path)),
        ]),
        Line::from(vec![
            Span::styled("parsed:  ", dim),
            Span::raw(file.title.clone().unwrap_or_else(|| "(none)".to_string())),
            Span::styled(
                file.episode.as_ref().map(|e| format!("  ep.{}", e)).unwrap_or_default(),
                dim,
            ),
            Span::styled(
                file.release_group.as_ref()
                    .map(|g| format!("  [{}]", g))
                    .unwrap_or_default(),
                dim,
            ),
        ]),
    ];

    if let Some(al) = &entry.sort.anilist {
        lines.push(Line::from(vec![
            Span::styled("anilist: ", dim),
            Span::styled(
                al.romaji.clone().unwrap_or_else(|| "?".to_string()),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled(
                al.english.as_ref()
                    .map(|e| format!("  /  {}", e))
                    .unwrap_or_default(),
                dim,
            ),
        ]));
    }

    if !file.video_time.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("at:      ", dim),
            Span::raw(file.video_time.clone()),
        ]));
    }

    if let Some(src) = &entry.sort.source {
        if let Some(matched) = src.matched_title() {
            lines.push(Line::from(vec![
                Span::styled("via:     ", dim),
                Span::raw(matched.to_owned()),
            ]));
        }
    }

    let (dest_str, dest_color) = match entry.effective_destination() {
        Destination::Existing(p) => (format!("→  {}", p.display()), Color::Green),
        Destination::New(n) => (format!("→  [CREATE] {}/", n), Color::Yellow),
        Destination::Unresolved => ("→  not resolved".to_string(), Color::Red),
    };
    lines.push(Line::from(Span::styled(dest_str, Style::default().fg(dest_color))));

    lines
}

fn draw_statusbar(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    let k = |s: &'static str| Span::styled(s, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let t = |s: &'static str| Span::raw(s);

    let controls = Paragraph::new(Line::from(vec![
        k("↑↓"), t(" nav  "),
        k("→←"), t(" expand  "),
        k("a"), t(" ok  "),
        k("A"), t(" all  "),
        k("e"), t(" edit  "),
        k("s"), t(" skip  "),
        k("c"), t(" commit  "),
        k("q"), t(" quit"),
    ]))
    .block(Block::default().borders(Borders::ALL));

    let stats_str = app.message.clone().unwrap_or_else(|| {
        format!(
            "total {}  ✓ {}  ~ {}  ! {}",
            app.entries.len(),
            app.approved_count(),
            app.pending_count(),
            app.needs_attention_count(),
        )
    });

    let stats = Paragraph::new(stats_str)
        .block(Block::default().borders(Borders::ALL).title(" Status "));

    f.render_widget(controls, chunks[0]);
    f.render_widget(stats, chunks[1]);
}

fn draw_edit_overlay(f: &mut Frame, state: &EditState, area: Rect) {
    let popup = centered_rect(62, 3, area);
    f.render_widget(Clear, popup);

    let (before, cursor_char, after) = split_at_cursor(&state.text, state.cursor);

    let para = Paragraph::new(Line::from(vec![
        Span::raw(before),
        Span::styled(cursor_char, Style::default().bg(Color::White).fg(Color::Black)),
        Span::raw(after),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Edit destination  (Enter=confirm  Esc=cancel) ")
            .border_style(Style::default().fg(Color::Yellow)),
    );

    f.render_widget(para, popup);
}

fn draw_confirm_overlay(f: &mut Frame, app: &App, area: Rect) {
    let new_count = app.entries.iter()
        .filter(|e| e.status == EntryStatus::Approved)
        .filter(|e| matches!(e.effective_destination(), Destination::New(_)))
        .count();

    let suffix = if new_count > 0 {
        format!(" and create {} new folder(s)", new_count)
    } else {
        String::new()
    };

    let text = format!(
        "Move {} file(s){}?\n\nPress y to confirm, n to cancel.",
        app.approved_count(),
        suffix,
    );

    let popup = centered_rect(50, 7, area);
    f.render_widget(Clear, popup);

    f.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Confirm ")
                    .border_style(Style::default().fg(Color::Yellow)),
            )
            .wrap(Wrap { trim: true }),
        popup,
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Final component of a path as an owned String (a folder or file name).
fn base_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Render a destination as a colored label for the list and group-header views.
/// (The detail panel shows the full path instead, so it doesn't use this.)
fn dest_display(dest: &Destination) -> (String, Color) {
    match dest {
        Destination::Existing(p) => (base_name(p), Color::Green),
        Destination::New(name)   => (format!("[NEW] {}/", name), Color::Yellow),
        Destination::Unresolved  => ("UNRESOLVED".to_string(), Color::Red),
    }
}

/// Split `text` into (before_cursor, char_at_cursor, after_cursor).
/// If the cursor is past the end, `char_at_cursor` is a space (block cursor).
fn split_at_cursor(text: &str, cursor: usize) -> (String, String, String) {
    let chars: Vec<char> = text.chars().collect();
    let before: String = chars[..cursor.min(chars.len())].iter().collect();
    let at: String = chars.get(cursor).map_or_else(|| " ".to_string(), |c| c.to_string());
    let after: String = chars.get(cursor + 1..).map(|s| s.iter().collect()).unwrap_or_default();
    (before, at, after)
}

/// Horizontally centred popup of fixed height and `percent_x`% width.
fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vert[1])[1]
}
