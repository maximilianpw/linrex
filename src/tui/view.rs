use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap,
};

use crate::checkout::Vcs;
use crate::config::RepoMatch;
use crate::issue_session::session_label;
use crate::linear::Priority;

use super::app::{
    App, CheckoutPreview, Comments, Compose, Dispatch, DispatchHitbox, Entry, Hitboxes, Mode,
    NoticeKind, PickerHitbox, WorkspaceLoad, describe_checkout, tilde,
};
use super::picker::Picker;
use super::theme;

const ACCENT: Color = theme::MAUVE;
const YELLOW: Color = theme::YELLOW;
const DIM: Color = theme::OVERLAY1;
const SESSION: Color = theme::GREEN;
const SELECTED_BG: Color = theme::SURFACE0;
const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

const MAIN_KEYS: &str = "↵ open · tab workspace · ^s status · ^t assign · ^p priority · ^e comment · ^n new · ^o browser · ^f project · ^u ^d scroll · ^r refresh · esc quit";
const DISPATCH_KEYS: &str =
    "↵ start · ↑↓ agent · tab repo · ^w checkout · alt-↵ newline · esc back";

pub fn draw(frame: &mut Frame, app: &mut App, frame_count: usize) {
    let keys = if matches!(app.mode, Mode::Dispatch(_)) {
        DISPATCH_KEYS
    } else {
        MAIN_KEYS
    };
    let width = frame.area().width.max(1) as usize;
    let key_lines = keys.chars().count().div_ceil(width) as u16;
    let notice_lines = app
        .notice
        .as_ref()
        .map_or(0, |n| (n.text.chars().count() + 1).div_ceil(width) as u16);
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(key_lines + notice_lines),
    ])
    .areas(frame.area());
    frame.render_widget(
        Block::new().style(Style::new().bg(theme::BASE).fg(theme::TEXT)),
        frame.area(),
    );
    app.hitboxes = Hitboxes::default();
    app.hitboxes.tabs = draw_header(frame, app, header, frame_count);
    draw_footer(frame, app, footer, keys);

    if let Mode::Dispatch(dispatch) = &app.mode {
        let hit = draw_dispatch(frame, app, dispatch, body);
        app.hitboxes.dispatch = hit;
        return;
    }
    let list_height = (app.visible.len() as u16 + 1).clamp(4, body.height * 45 / 100);
    let [list, rule, detail] = Layout::vertical([
        Constraint::Length(list_height.max(4)),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(body);
    draw_list(frame, app, list);
    frame.render_widget(
        Block::new()
            .borders(Borders::TOP)
            .border_style(Style::new().fg(DIM)),
        rule,
    );
    draw_detail(frame, app, detail);
    app.hitboxes.detail = detail;

    match &app.mode {
        Mode::Picker(picker) => app.hitboxes.picker = Some(draw_picker(frame, picker)),
        Mode::Compose(compose, editor) => {
            let title = match compose {
                Compose::Comment { identifier, .. } => format!(" Comment on {identifier} "),
                Compose::NewIssue { team_key, .. } => format!(" New {team_key} issue "),
            };
            let area = popup(frame.area(), 70, 14);
            frame.render_widget(Clear, area);
            let block = Block::bordered()
                .style(Style::new().bg(theme::MANTLE).fg(theme::TEXT))
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(ACCENT))
                .title(title)
                .title_bottom(Line::from(" ctrl-s send · esc cancel ").fg(DIM));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(editor.as_ref(), inner);
        }
        Mode::Normal | Mode::Dispatch(_) => {}
    }
}

fn separator() -> Span<'static> {
    Span::styled(" · ", Style::new().fg(DIM))
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect, frame_count: usize) -> Vec<(Rect, usize)> {
    let mut spans = vec![Span::styled(" linrex ", Style::new().fg(ACCENT).bold())];
    let mut tabs = Vec::new();
    let mut x = area.x + spans[0].width() as u16;
    for (index, workspace) in app.workspaces.iter().enumerate() {
        let status = match &workspace.load {
            WorkspaceLoad::Loading => SPINNER[frame_count % SPINNER.len()].to_owned(),
            WorkspaceLoad::Ready { .. } => app.workspace_issue_count(index).to_string(),
            WorkspaceLoad::Failed(_) => "✕".to_owned(),
        };
        let text = format!(" {} {status} ", workspace.name);
        let width = Span::raw(text.as_str()).width() as u16;
        tabs.push((Rect::new(x, area.y, width, 1), index));
        x += width;
        spans.push(if index == app.workspace {
            Span::styled(text, Style::new().fg(theme::CRUST).bg(ACCENT).bold())
        } else if matches!(workspace.load, WorkspaceLoad::Failed(_)) {
            Span::styled(text, Style::new().fg(theme::RED))
        } else {
            Span::styled(text, Style::new().fg(DIM))
        });
    }
    spans.extend([
        separator(),
        match &app.project {
            Some(project) => Span::styled(project.clone(), Style::new().fg(YELLOW).bold()),
            None => Span::styled("all projects", Style::new().fg(DIM)),
        },
        separator(),
        Span::styled(
            format!(
                "{}/{}",
                app.visible.len(),
                app.workspace_issue_count(app.workspace)
            ),
            Style::new().fg(DIM),
        ),
    ]);
    if app.busy > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            SPINNER[frame_count % SPINNER.len()],
            Style::new().fg(ACCENT),
        ));
    }
    frame.render_widget(Line::from(spans), area);
    tabs
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let [search, rows_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    let search_line = if app.query.is_empty() {
        Line::from(vec![
            Span::styled(" › ", Style::new().fg(ACCENT)),
            Span::styled("▏", Style::new().fg(ACCENT)),
            Span::styled("fuzzy search your issues…", Style::new().fg(DIM)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" › ", Style::new().fg(ACCENT)),
            Span::raw(app.query.clone()),
            Span::styled("▏", Style::new().fg(ACCENT)),
        ])
    };
    frame.render_widget(search_line, search);

    let rows: Vec<Row> = app
        .visible
        .iter()
        .map(|&index| {
            let issue = &app.entries[index].issue;
            let marker = if app.starting.contains(&issue.identifier) {
                Span::styled("…", Style::new().fg(ACCENT))
            } else if app.session_for(issue).is_some() {
                Span::styled("●", Style::new().fg(SESSION))
            } else {
                Span::raw(" ")
            };
            Row::new([
                Cell::from(marker),
                Cell::from(Span::styled(issue.identifier.clone(), Style::new().fg(DIM))),
                Cell::from(Span::styled(
                    issue.state.name.clone(),
                    Style::new().fg(theme::status_color(&issue.state.color)),
                )),
                Cell::from(priority_span(issue.priority)),
                Cell::from(issue.title.clone()),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(1),
        Constraint::Length(9),
        Constraint::Length(12),
        Constraint::Length(2),
        Constraint::Fill(1),
    ];
    let empty = app.visible.is_empty();
    let table = Table::new(rows, widths)
        .column_spacing(1)
        .row_highlight_style(Style::new().bg(SELECTED_BG).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(table, rows_area, &mut app.table);
    app.hitboxes.list = rows_area;
    app.hitboxes.list_offset = app.table.offset();
    if empty {
        let message = if matches!(app.workspaces[app.workspace].load, WorkspaceLoad::Loading) {
            "Loading your issues…"
        } else if !app.query.is_empty() || app.project.is_some() {
            "No issues match"
        } else {
            "No open issues assigned to you"
        };
        frame.render_widget(Line::from(format!("   {message}")).fg(DIM), rows_area);
    }
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(entry) = app.selected() else {
        let workspace = &app.workspaces[app.workspace];
        if let WorkspaceLoad::Failed(error) = &workspace.load {
            frame.render_widget(
                Paragraph::new(Line::from(format!("{}: {error}", workspace.name)).fg(theme::RED))
                    .wrap(Wrap { trim: false }),
                area,
            );
        }
        return;
    };
    let inner = area.inner(ratatui::layout::Margin::new(1, 0));
    frame.render_widget(
        Paragraph::new(detail_text(app, entry))
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        inner,
    );
}

fn meta_line(entry: &Entry) -> Line<'static> {
    let issue = &entry.issue;
    let mut spans = vec![
        Span::styled(
            issue.state.name.clone(),
            Style::new()
                .fg(theme::status_color(&issue.state.color))
                .bold(),
        ),
        separator(),
        Span::styled(issue.priority.label(), Style::new().fg(DIM)),
        separator(),
        Span::styled(
            issue
                .assignee
                .as_ref()
                .map_or("Unassigned".into(), |a| a.display_name.clone()),
            Style::new().fg(DIM),
        ),
        separator(),
        Span::styled(issue.team.key.clone(), Style::new().fg(DIM)),
    ];
    if let Some(project) = &issue.project {
        spans.push(separator());
        spans.push(Span::styled(project.clone(), Style::new().fg(YELLOW)));
    }
    Line::from(spans)
}

fn detail_text(app: &App, entry: &Entry) -> Text<'static> {
    let issue = &entry.issue;
    let mut lines = vec![
        Line::from(issue.identifier.clone()).fg(DIM),
        Line::from(issue.title.clone()).bold(),
        meta_line(entry),
    ];
    if !issue.labels.is_empty() {
        let tags: Vec<String> = issue.labels.iter().map(|l| format!("#{l}")).collect();
        lines.push(Line::from(tags.join(" ")).fg(DIM));
    }
    lines.push(session_line(app, entry));
    lines.push(Line::default());
    match &issue.description {
        Some(description) => lines.extend(markdown_lines(description)),
        None => lines.push(Line::from("No description").fg(DIM)),
    }
    lines.push(Line::default());
    match app.comments.get(&entry.key()) {
        Some(Comments::Loaded(comments)) if comments.is_empty() => {}
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
            lines.push(Line::from(format!("Comments: {error}")).fg(theme::RED))
        }
        Some(Comments::Loading) | None => lines.push(Line::from("Loading comments…").fg(DIM)),
    }
    Text::from(lines)
}

fn session_line(app: &App, entry: &Entry) -> Line<'static> {
    if let Some(session) = app.session_for(&entry.issue) {
        return Line::from(vec![
            Span::styled("● ", Style::new().fg(SESSION)),
            Span::styled(session.label.clone(), Style::new().fg(SESSION)),
            Span::styled("  ↵ switches to it", Style::new().fg(DIM)),
        ]);
    }
    let checkout = match &app.checkout_preview {
        Some(CheckoutPreview { key, text }) if *key == entry.key() => text.clone(),
        _ => match app.repo_match(entry) {
            RepoMatch::One(_) => "…".into(),
            RepoMatch::Ambiguous(repos) => format!("{} matching repos", repos.len()),
            RepoMatch::Unmapped => format!("team {} has no mapped repo", entry.issue.team.key),
        },
    };
    Line::from(vec![
        Span::styled("○ no session", Style::new().fg(DIM)),
        Span::styled(format!("  ↵ starts one · {checkout}"), Style::new().fg(DIM)),
    ])
}

fn draw_dispatch(
    frame: &mut Frame,
    app: &App,
    dispatch: &Dispatch,
    area: Rect,
) -> Option<DispatchHitbox> {
    let entry = app.entry(&dispatch.target)?;
    let issue = &entry.issue;
    let label_style = Style::new().fg(DIM);
    let label = |text: &str| Span::styled(format!("{text:<10}"), label_style);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{} ", issue.identifier), Style::new().fg(DIM)),
            Span::raw(issue.title.clone()).bold(),
        ]),
        Line::from(vec![
            label("Session"),
            Span::raw(session_label(issue)),
            Span::styled("  new Rex session", Style::new().fg(DIM)),
        ]),
        Line::default(),
    ];
    let agents_line_index = lines.len() as u16;
    for (index, agent) in app.agents().iter().enumerate() {
        let chosen = index == dispatch.agent;
        lines.push(Line::from(vec![
            if index == 0 {
                label("Agent")
            } else {
                label("")
            },
            Span::styled(if chosen { "› " } else { "  " }, Style::new().fg(ACCENT)),
            Span::styled(
                format!("{:<18}", agent.name),
                if chosen {
                    Style::new().fg(ACCENT).bold()
                } else {
                    Style::new()
                },
            ),
            Span::styled(agent.command.join(" "), Style::new().fg(DIM)),
        ]));
    }
    lines.push(Line::default());
    let repo = dispatch.repo();
    let mut repo_line = vec![label("Repo"), Span::raw(tilde(&repo.path))];
    if dispatch.repos.len() > 1 {
        repo_line.push(Span::styled(
            format!(
                "  {}/{} · tab for next",
                dispatch.repo + 1,
                dispatch.repos.len()
            ),
            Style::new().fg(DIM),
        ));
    }
    let repo_line_index = lines.len() as u16;
    lines.push(Line::from(repo_line));
    if repo.vcs() == Vcs::None {
        lines.push(Line::from(vec![
            label("Checkout"),
            Span::styled("plain folder, shared by all issues", Style::new().fg(DIM)),
        ]));
    } else {
        lines.push(Line::from(vec![
            label("Checkout"),
            Span::styled(
                if dispatch.per_issue_checkout {
                    "[x] "
                } else {
                    "[ ] "
                },
                Style::new().fg(ACCENT),
            ),
            Span::raw(format!(
                "per-issue {}",
                match repo.vcs() {
                    Vcs::Jj => "jj workspace",
                    _ => "git worktree",
                }
            )),
            Span::styled("  ^w toggles", Style::new().fg(DIM)),
        ]));
        lines.push(Line::from(vec![
            label(""),
            Span::styled(
                if dispatch.per_issue_checkout {
                    format!("    {}", describe_checkout(&dispatch.checkout))
                } else {
                    format!("    {} · main checkout", tilde(&dispatch.checkout.path))
                },
                Style::new().fg(DIM),
            ),
        ]));
    }
    if app.moves_to_started() && issue.state.kind.is_not_started() {
        lines.push(Line::from(vec![
            label("Status"),
            Span::raw(format!("{} → first started state", issue.state.name)),
        ]));
    }
    let info_height = lines.len() as u16 + 1;
    let [info, notes] = Layout::vertical([Constraint::Length(info_height), Constraint::Min(3)])
        .areas(area.inner(ratatui::layout::Margin::new(1, 0)));
    frame.render_widget(Paragraph::new(lines), info);
    let block = Block::bordered()
        .style(Style::new().bg(theme::MANTLE).fg(theme::TEXT))
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(" Notes for the agent (optional) ");
    let inner = block.inner(notes);
    frame.render_widget(block, notes);
    frame.render_widget(&dispatch.notes, inner);
    let row = |line: u16, height: u16| Rect::new(info.x, info.y + line, info.width, height);
    Some(DispatchHitbox {
        agents: row(agents_line_index, app.agents().len() as u16),
        repo: row(repo_line_index, 1),
        checkout: row(repo_line_index + 1, 2),
    })
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect, keys: &str) {
    let mut lines = Vec::new();
    if let Some(notice) = &app.notice {
        let color = match notice.kind {
            NoticeKind::Info => theme::BLUE,
            NoticeKind::Error => theme::RED,
        };
        lines.push(Line::from(format!(" {}", notice.text)).fg(color));
    }
    lines.push(Line::from(keys).fg(DIM));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn priority_span(priority: Priority) -> Span<'static> {
    match priority {
        Priority::Urgent => Span::styled("!!", Style::new().fg(theme::RED).bold()),
        Priority::High => Span::styled("▲ ", Style::new().fg(theme::PEACH)),
        Priority::Medium => Span::styled("■ ", Style::new().fg(theme::YELLOW)),
        Priority::Low => Span::styled("▼ ", Style::new().fg(DIM)),
        Priority::None => Span::styled("  ", Style::new()),
    }
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
                return Line::from(line.to_owned()).fg(theme::GREEN);
            }
            if line.starts_with('#') {
                return Line::from(line.trim_start_matches('#').trim().to_owned())
                    .fg(theme::LAVENDER)
                    .bold();
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
fn popup(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}
fn draw_picker(frame: &mut Frame, picker: &Picker) -> PickerHitbox {
    let matches = picker.matches();
    let height = (matches.len() as u16 + 4).clamp(6, 20);
    let area = popup(frame.area(), 60, height);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .style(Style::new().bg(theme::MANTLE).fg(theme::TEXT))
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
            let label_style = item.color.map_or(Style::new(), |c| Style::new().fg(c));
            ListItem::new(Line::from(vec![
                Span::styled(if item.current { "● " } else { "  " }, label_style),
                Span::styled(item.label.clone(), label_style),
                Span::raw(format!("  {}", item.hint)).fg(DIM),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(picker.selected));
    frame.render_stateful_widget(
        List::new(items).highlight_style(Style::new().bg(SELECTED_BG).fg(theme::TEXT).bold()),
        list,
        &mut state,
    );
    PickerHitbox {
        popup: area,
        list,
        offset: state.offset(),
    }
}
