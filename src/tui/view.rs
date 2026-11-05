use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};

use crate::config::RepoMatch;
use crate::linear::{Issue, Priority, StateType};

use super::app::{
    App, CheckoutPreview, Comments, Compose, Entry, Mode, NoticeKind, View, WorkspaceLoad,
};
use super::picker::Picker;

const ACCENT: Color = Color::Rgb(122, 162, 247);
const DIM: Color = Color::Rgb(110, 115, 135);
const SESSION: Color = Color::Rgb(120, 200, 120);
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

pub fn draw(frame: &mut Frame, app: &mut App, frame_count: usize) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_header(frame, app, header, frame_count);
    let [list, detail] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body);
    draw_list(frame, app, list);
    draw_detail(frame, app, detail);
    draw_footer(frame, app, footer);

    match &app.mode {
        Mode::Picker(picker) => draw_picker(frame, picker),
        Mode::Compose(compose, editor) => {
            let title = match compose {
                Compose::Comment { identifier, .. } => format!(" Comment on {identifier} "),
                Compose::NewIssue { team_key, .. } => format!(" New {team_key} issue "),
            };
            let area = popup(frame.area(), 70, 14);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(ACCENT))
                .title(title)
                .title_bottom(Line::from(" ctrl-s send · esc cancel ").fg(DIM));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(editor.as_ref(), inner);
        }
        Mode::Help => draw_help(frame),
        Mode::Normal | Mode::Filter => {}
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect, frame_count: usize) {
    let tab = |label: &str, view: View| {
        if app.view == view {
            Span::styled(
                format!(" {label} "),
                Style::new().fg(Color::Black).bg(ACCENT).bold(),
            )
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(DIM))
        }
    };
    let mut spans = vec![
        Span::styled(" linrex ", Style::new().fg(ACCENT).bold()),
        tab("My issues", View::Mine),
        tab("Team", View::Team),
        Span::raw("  "),
    ];
    for workspace in &app.workspaces {
        let (marker, style) = match &workspace.load {
            WorkspaceLoad::Loading => (SPINNER[frame_count % SPINNER.len()], Style::new().fg(DIM)),
            WorkspaceLoad::Ready { .. } => ("●", Style::new().fg(SESSION)),
            WorkspaceLoad::Failed(_) => ("✕", Style::new().fg(Color::Red)),
        };
        spans.push(Span::styled(format!("{marker} {} ", workspace.name), style));
    }
    if app.busy > 0 {
        spans.push(Span::styled(
            SPINNER[frame_count % SPINNER.len()],
            Style::new().fg(ACCENT),
        ));
    }
    if !app.filter.is_empty() || matches!(app.mode, Mode::Filter) {
        spans.push(Span::raw("   / ").fg(DIM));
        spans.push(Span::raw(app.filter.clone()).bold());
        if matches!(app.mode, Mode::Filter) {
            spans.push(Span::raw("▏").fg(ACCENT));
        }
    }
    frame.render_widget(Line::from(spans), area);
}

fn state_glyph(kind: StateType) -> &'static str {
    match kind {
        StateType::Triage => "◇",
        StateType::Backlog => "◌",
        StateType::Unstarted => "○",
        StateType::Started => "◐",
        StateType::Completed => "●",
        StateType::Canceled => "⊘",
        StateType::Other => "·",
    }
}

fn priority_span(priority: Priority) -> Span<'static> {
    match priority {
        Priority::Urgent => Span::styled("!!", Style::new().fg(Color::Red).bold()),
        Priority::High => Span::styled("▲ ", Style::new().fg(Color::Rgb(240, 150, 80))),
        Priority::Medium => Span::styled("■ ", Style::new().fg(Color::Rgb(200, 200, 120))),
        Priority::Low => Span::styled("▼ ", Style::new().fg(DIM)),
        Priority::None => Span::styled("  ", Style::new()),
    }
}

fn hex_color(hex: &str) -> Color {
    let hex = hex.trim_start_matches('#');
    match (
        u8::from_str_radix(hex.get(0..2).unwrap_or(""), 16),
        u8::from_str_radix(hex.get(2..4).unwrap_or(""), 16),
        u8::from_str_radix(hex.get(4..6).unwrap_or(""), 16),
    ) {
        (Ok(r), Ok(g), Ok(b)) => Color::Rgb(r, g, b),
        _ => Color::Gray,
    }
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let multi = app.multi_workspace();
    let show_assignee = app.view == View::Team && area.width >= 80;
    let rows: Vec<Row> = app
        .visible
        .iter()
        .map(|&index| {
            let entry = &app.entries[index];
            let issue = &entry.issue;
            let has_session = app.session_for(issue).is_some();
            let starting = app.starting.contains(&issue.identifier);
            let marker = if starting {
                Span::styled("…", Style::new().fg(ACCENT))
            } else if has_session {
                Span::styled("●", Style::new().fg(SESSION))
            } else {
                Span::raw(" ")
            };
            let mut cells = vec![
                Cell::from(marker),
                Cell::from(Span::styled(issue.identifier.clone(), Style::new().fg(DIM))),
                Cell::from(Line::from(vec![
                    Span::styled(
                        state_glyph(issue.state.kind),
                        Style::new().fg(hex_color(&issue.state.color)),
                    ),
                    Span::raw(" "),
                    Span::raw(issue.state.name.clone()),
                ])),
                Cell::from(priority_span(issue.priority)),
                Cell::from(issue.title.clone()),
            ];
            if show_assignee {
                cells.push(Cell::from(
                    Span::raw(
                        issue
                            .assignee
                            .as_ref()
                            .map_or("—".into(), |a| a.display_name.clone()),
                    )
                    .fg(DIM),
                ));
            }
            if multi {
                cells.push(Cell::from(
                    Span::raw(app.workspaces[entry.workspace].name.clone()).fg(DIM),
                ));
            }
            Row::new(cells)
        })
        .collect();
    let mut widths = vec![
        Constraint::Length(1),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Length(2),
        Constraint::Fill(1),
    ];
    if show_assignee {
        widths.push(Constraint::Length(12));
    }
    if multi {
        widths.push(Constraint::Length(8));
    }
    let title = format!(
        " {} issue{} ",
        app.visible.len(),
        if app.visible.len() == 1 { "" } else { "s" }
    );
    let empty = app.visible.is_empty();
    let table = Table::new(rows, widths)
        .column_spacing(1)
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(DIM))
                .title(title),
        )
        .row_highlight_style(
            Style::new()
                .bg(Color::Rgb(45, 50, 70))
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(table, area, &mut app.table);
    if empty {
        let loading = app
            .workspaces
            .iter()
            .any(|w| matches!(w.load, WorkspaceLoad::Loading));
        let message = if loading {
            "Loading issues…"
        } else if !app.filter.is_empty() {
            "No issues match the filter"
        } else {
            "No issues"
        };
        let inner = Block::bordered().inner(area);
        frame.render_widget(Paragraph::new(message).fg(DIM).centered(), inner);
    }
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM));
    let Some(entry) = app.selected() else {
        let failures: Vec<Line> = app
            .workspaces
            .iter()
            .filter_map(|w| match &w.load {
                WorkspaceLoad::Failed(error) => {
                    Some(Line::from(format!("{}: {error}", w.name)).fg(Color::Red))
                }
                _ => None,
            })
            .collect();
        frame.render_widget(
            Paragraph::new(failures)
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
        return;
    };
    let text = detail_text(app, entry);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0))
            .block(block.title(format!(" {} ", entry.issue.identifier))),
        area,
    );
}

fn field(name: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{name:<10}"), Style::new().fg(DIM)),
        Span::raw(value.into()),
    ])
}

fn detail_text(app: &App, entry: &Entry) -> Text<'static> {
    let issue: &Issue = &entry.issue;
    let mut lines = vec![
        Line::from(issue.title.clone()).bold(),
        Line::default(),
        Line::from(vec![
            Span::styled(format!("{:<10}", "Status"), Style::new().fg(DIM)),
            Span::styled(
                state_glyph(issue.state.kind),
                Style::new().fg(hex_color(&issue.state.color)),
            ),
            Span::raw(format!(" {}", issue.state.name)),
        ]),
        field("Priority", issue.priority.label()),
        field(
            "Assignee",
            issue
                .assignee
                .as_ref()
                .map_or("Unassigned".into(), |a| a.display_name.clone()),
        ),
        field("Team", format!("{} · {}", issue.team.key, issue.team.name)),
    ];
    if let Some(project) = &issue.project {
        lines.push(field("Project", project.clone()));
    }
    if !issue.labels.is_empty() {
        lines.push(field("Labels", issue.labels.join(", ")));
    }
    if app.multi_workspace() {
        lines.push(field(
            "Workspace",
            app.workspaces[entry.workspace].name.clone(),
        ));
    }
    lines.push(Line::default());
    lines.extend(session_lines(app, entry));
    lines.push(Line::default());
    lines.push(Line::from("Description").fg(ACCENT).bold());
    match &issue.description {
        Some(description) => lines.extend(markdown_lines(description)),
        None => lines.push(Line::from("No description").fg(DIM)),
    }
    lines.push(Line::default());
    match app.comments.get(&entry.key()) {
        Some(Comments::Loaded(comments)) if comments.is_empty() => {
            lines.push(Line::from("No comments").fg(DIM));
        }
        Some(Comments::Loaded(comments)) => {
            lines.push(
                Line::from(format!("Comments ({})", comments.len()))
                    .fg(ACCENT)
                    .bold(),
            );
            for comment in comments {
                lines.push(Line::default());
                lines.push(Line::from(vec![
                    Span::raw(
                        comment
                            .user
                            .as_ref()
                            .map_or("Unknown".into(), |u| u.display_name.clone()),
                    )
                    .bold(),
                    Span::raw(format!(
                        "  {}",
                        comment.created_at.get(..10).unwrap_or(&comment.created_at)
                    ))
                    .fg(DIM),
                ]));
                lines.extend(markdown_lines(&comment.body));
            }
        }
        Some(Comments::Failed(error)) => {
            lines.push(Line::from(format!("Comments: {error}")).fg(Color::Red))
        }
        Some(Comments::Loading) | None => lines.push(Line::from("Loading comments…").fg(DIM)),
    }
    Text::from(lines)
}

fn session_lines(app: &App, entry: &Entry) -> Vec<Line<'static>> {
    if let Some(session) = app.session_for(&entry.issue) {
        return vec![
            Line::from(vec![
                Span::styled(format!("{:<10}", "Session"), Style::new().fg(DIM)),
                Span::styled("● ", Style::new().fg(SESSION)),
                Span::raw(session.label.clone()).bold(),
            ]),
            Line::from("          enter switches to it").fg(DIM),
        ];
    }
    let checkout = match &app.checkout_preview {
        Some(CheckoutPreview { key, text }) if *key == entry.key() => text.clone(),
        _ => match app.repo_match(entry) {
            RepoMatch::One(_) => "…".into(),
            RepoMatch::Ambiguous(repos) => format!("pick one of {} repos", repos.len()),
            RepoMatch::Unmapped => format!("team {} has no mapped repo", entry.issue.team.key),
        },
    };
    vec![
        field("Session", "none · enter starts one"),
        field("Checkout", checkout),
    ]
}

fn markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut in_code = false;
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                in_code = !in_code;
                return Line::from(line.to_owned()).fg(DIM);
            }
            if in_code {
                return Line::from(line.to_owned()).fg(Color::Rgb(180, 190, 140));
            }
            if line.starts_with('#') {
                return Line::from(line.trim_start_matches('#').trim().to_owned()).bold();
            }
            if let Some(rest) = line
                .trim_start()
                .strip_prefix("- ")
                .or(line.trim_start().strip_prefix("* "))
            {
                let indent = line.len() - line.trim_start().len();
                return Line::from(format!("{}• {rest}", " ".repeat(indent)));
            }
            Line::from(line.to_owned())
        })
        .collect()
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = match (&app.notice, &app.mode) {
        (_, Mode::Filter) => Line::from(" type to filter · enter keep · esc clear").fg(DIM),
        (Some(notice), _) => {
            let color = match notice.kind {
                NoticeKind::Info => ACCENT,
                NoticeKind::Error => Color::Red,
            };
            Line::from(format!(" {}", notice.text)).fg(color)
        }
        (None, _) => Line::from(
            " enter session · s status · a assign · p priority · c comment · n new · o open · / filter · tab view · ? help",
        )
        .fg(DIM),
    };
    frame.render_widget(line, area);
}

fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn draw_picker(frame: &mut Frame, picker: &Picker) {
    let matches = picker.matches();
    let height = (matches.len() as u16 + 4).clamp(6, 20);
    let area = popup(frame.area(), 60, height);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(format!(" {} ", picker.title));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [query, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
    frame.render_widget(
        Line::from(vec![
            Span::raw("› ").fg(ACCENT),
            Span::raw(picker.query.clone()),
            Span::raw("▏").fg(ACCENT),
        ]),
        query,
    );
    let items: Vec<ListItem> = matches
        .iter()
        .map(|item| {
            ListItem::new(Line::from(vec![
                Span::raw(item.label.clone()),
                Span::raw(format!("  {}", item.hint)).fg(DIM),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::new().bg(Color::Rgb(45, 50, 70)).bold()),
        list,
        &mut state,
    );
}

fn draw_help(frame: &mut Frame) {
    let keys = [
        ("enter", "switch to the issue's Rex session, or start one"),
        ("j/k ↑/↓", "move · g/G top/bottom · ctrl-d/u page"),
        ("J/K", "scroll the detail pane"),
        ("tab", "toggle My issues / Team"),
        ("/", "filter · esc clears"),
        ("s a p", "set status, assignee, priority"),
        ("c", "comment on the issue"),
        ("n", "create an issue"),
        ("o", "open in the browser"),
        ("r", "reload from Linear"),
        ("q", "quit"),
    ];
    let lines: Vec<Line> = keys
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::raw(format!("{key:>9}  ")).fg(ACCENT).bold(),
                Span::raw(*what),
            ])
        })
        .collect();
    let area = popup(frame.area(), 64, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(ACCENT))
                .title(" Keys "),
        ),
        area,
    );
}
