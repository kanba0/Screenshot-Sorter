use std::io;
use std::path::PathBuf;
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

use crate::app::{App, EditState, EntryStatus, Mode};
use crate::matching::{Destination, SortEntry};

// ── Entry point ───────────────────────────────────────────────────────────────

pub fn run(entries: Vec<SortEntry>, dest_root: PathBuf) -> Result<()> {
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

    if let Some(msg) = app.message {
        println!("{}", msg);
    }

    Ok(())
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
    match key {
        KeyCode::Char(c)   => app.edit_insert(c),
        KeyCode::Backspace => app.edit_backspace(),
        KeyCode::Delete    => app.edit_delete(),
        KeyCode::Left      => app.edit_cursor_left(),
        KeyCode::Right     => app.edit_cursor_right(),
        KeyCode::Home      => app.edit_cursor_home(),
        KeyCode::End       => app.edit_cursor_end(),
        KeyCode::Enter     => app.confirm_edit(),
        KeyCode::Esc       => app.cancel_edit(),
        _ => {}
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
    let max_fn_width = (area.width as usize).saturating_sub(36);

    let items: Vec<ListItem> = app.entries.iter().map(|entry| {
        let (status_char, mut status_color) = match entry.status {
            EntryStatus::Approved => ("✓", Color::Green),
            EntryStatus::Pending  => ("~", Color::Yellow),
            EntryStatus::Skipped  => ("s", Color::DarkGray),
        };

        let (dest_str, dest_color) = match entry.effective_destination() {
            Destination::Existing(p) => (
                p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                Color::Green,
            ),
            Destination::New(name) => (format!("[NEW] {}/", name), Color::Yellow),
            Destination::Unresolved => ("UNRESOLVED".to_string(), Color::Red),
        };

        // Unresolved pending entries get red status indicator too
        if entry.status == EntryStatus::Pending
            && matches!(entry.effective_destination(), Destination::Unresolved)
        {
            status_color = Color::Red;
        }

        let filename = entry.sort.file.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Char-safe truncation (filenames can contain Japanese etc.)
        let filename_display = if filename.chars().count() > max_fn_width {
            let t: String = filename.chars().take(max_fn_width.saturating_sub(1)).collect();
            format!("{}…", t)
        } else {
            filename
        };

        let source_label = entry.sort.source.as_ref().map(|s| s.label()).unwrap_or("?");

        ListItem::new(Line::from(vec![
            Span::styled(format!("[{}] ", status_char), Style::default().fg(status_color)),
            Span::raw(filename_display),
            Span::raw(" → "),
            Span::styled(dest_str, Style::default().fg(dest_color)),
            Span::styled(
                format!(" [{}]", source_label),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
    }).collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Screenshots "))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, area, list_state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let lines = match app.entries.get(app.selected) {
        None => vec![Line::raw("")],
        Some(entry) => {
            let file = &entry.sort.file;
            let dim = Style::default().fg(Color::DarkGray);

            let mut lines = vec![
                Line::from(vec![
                    Span::styled("file:    ", dim),
                    Span::raw(
                        file.path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("parsed:  ", dim),
                    Span::raw(file.title.as_deref().unwrap_or("(none)")),
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
                        al.romaji.as_deref().unwrap_or("?"),
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
    };

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Details "))
            .wrap(Wrap { trim: true }),
        area,
    );
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
        k("a"), t(" approve  "),
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
