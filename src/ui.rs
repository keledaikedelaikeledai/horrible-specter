use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    app::App,
    input::{Focus, Mode, Overlay},
};

const BG: Color = Color::Rgb(15, 18, 24);
const PANEL: Color = Color::Rgb(22, 27, 35);
const PANEL_ALT: Color = Color::Rgb(28, 34, 44);
const TEXT: Color = Color::Rgb(221, 226, 234);
const MUTED: Color = Color::Rgb(133, 145, 162);
const ACCENT: Color = Color::Rgb(93, 202, 177);
const BLUE: Color = Color::Rgb(116, 170, 255);
const WARNING: Color = Color::Rgb(238, 184, 92);

pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(2)])
        .split(area);
    let content = vertical[0];
    let status = vertical[1];
    let show_right = app.right_sidebar_visible && content.width >= 110;
    let show_left = app.left_sidebar_visible && content.width >= 72;
    let constraints = match (show_left, show_right) {
        (true, true) => vec![
            Constraint::Length(28),
            Constraint::Min(40),
            Constraint::Length(34),
        ],
        (true, false) => vec![Constraint::Length(28), Constraint::Min(40)],
        (false, true) => vec![Constraint::Min(40), Constraint::Length(34)],
        (false, false) => vec![Constraint::Min(1)],
    };
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(content);
    let mut index = 0;
    if show_left {
        draw_left(app, frame, columns[index]);
        index += 1;
    }
    draw_center(app, frame, columns[index]);
    index += 1;
    if show_right {
        draw_right(app, frame, columns[index]);
    }
    draw_status(app, frame, status);
    if let Some(overlay) = &app.overlay {
        draw_overlay(frame, area, overlay);
    }
}

fn draw_left(app: &App, frame: &mut Frame, area: Rect) {
    let active_id = app.active_conversation_id();
    let items = app
        .conversations
        .iter()
        .map(|conversation| {
            let active = conversation.id == active_id;
            ListItem::new(Line::from(vec![
                Span::styled(
                    if active { "● " } else { "○ " },
                    Style::default().fg(if active { ACCENT } else { MUTED }),
                ),
                Span::styled(conversation.title.clone(), Style::default().fg(TEXT)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select((app.focus == Focus::Conversations).then_some(app.active_conversation));
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " PROJECT ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(app.project.display_name.clone(), Style::default().fg(TEXT)),
        ]))
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(PANEL_ALT))
        .style(Style::default().bg(PANEL));
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(PANEL_ALT))
        .highlight_symbol("  ");
    frame.render_stateful_widget(list, area, &mut state);
    let footer = Rect {
        x: area.x + 1,
        y: area.bottom().saturating_sub(3),
        width: area.width.saturating_sub(2),
        height: 2,
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled("CONVERSATIONS", Style::default().fg(MUTED))),
            Line::from(Span::styled(
                format!("{} sessions", app.conversations.len()),
                Style::default().fg(TEXT),
            )),
        ]),
        footer,
    );
}

fn draw_center(app: &App, frame: &mut Frame, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(area);
    let conversation = &app.conversations[app.active_conversation];
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                conversation.title.clone(),
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  ", Style::default().fg(MUTED)),
            Span::styled(app.providers.active_label(), Style::default().fg(WARNING)),
        ]),
        Line::from(Span::styled(
            "Application history is available across providers",
            Style::default().fg(MUTED),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(PANEL_ALT))
            .style(Style::default().bg(BG)),
    );
    frame.render_widget(header, layout[0]);

    let mut lines = Vec::new();
    if app.messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet",
            Style::default().fg(MUTED),
        )));
        lines.push(Line::from(Span::styled(
            "Press i to compose a message. Provider execution is not configured.",
            Style::default().fg(MUTED),
        )));
    } else {
        for message in &app.messages {
            let (label, color) = match message.role.as_str() {
                "user" => ("YOU", ACCENT),
                "assistant" => ("AGENT", BLUE),
                _ => ("SYSTEM", WARNING),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{label}  "),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled("·", Style::default().fg(MUTED)),
            ]));
            for content_line in message.content.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(content_line.to_string(), Style::default().fg(TEXT)),
                ]));
            }
            lines.push(Line::from(""));
        }
    }
    let message_height = layout[1].height as usize;
    let max_scroll = lines.len().saturating_sub(message_height);
    let start = app.scroll_offset.min(max_scroll);
    let visible = lines
        .into_iter()
        .skip(start)
        .take(message_height)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(visible)
            .style(Style::default().bg(BG))
            .wrap(Wrap { trim: false }),
        layout[1],
    );

    let composer_title = if app.mode == Mode::Compose {
        " COMPOSER · INSERT · SHIFT+ENTER NEWLINE "
    } else {
        " COMPOSER · PRESS i TO WRITE "
    };
    let composer_text = app.composer.as_str();
    let composer_lines = composer_text.lines().count().max(1);
    let composer_scroll =
        composer_lines.saturating_sub(layout[2].height.saturating_sub(2) as usize);
    let composer = Paragraph::new(composer_text)
        .scroll((composer_scroll as u16, 0))
        .style(Style::default().fg(if app.mode == Mode::Compose {
            TEXT
        } else {
            MUTED
        }))
        .block(
            Block::default()
                .title(Span::styled(
                    composer_title,
                    Style::default().fg(if app.mode == Mode::Compose {
                        ACCENT
                    } else {
                        MUTED
                    }),
                ))
                .borders(Borders::TOP)
                .border_style(Style::default().fg(if app.mode == Mode::Compose {
                    ACCENT
                } else {
                    PANEL_ALT
                }))
                .style(Style::default().bg(PANEL)),
        );
    frame.render_widget(composer, layout[2]);
}

fn draw_right(app: &App, frame: &mut Frame, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Min(4),
        ])
        .split(area);
    let branch = app.workspace.branch.as_deref().unwrap_or("detached");
    let summary = if let Some(error) = &app.workspace.error {
        error.clone()
    } else if app.workspace.is_git_repo {
        format!("{} changed file(s)", app.workspace.files.len())
    } else {
        "Workspace has no Git metadata".to_string()
    };
    let status = Paragraph::new(vec![
        Line::from(Span::styled(
            "WORKSPACE",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("⎇ {branch}"),
            Style::default().fg(TEXT),
        )),
        Line::from(Span::styled(summary, Style::default().fg(MUTED))),
    ])
    .block(
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(PANEL_ALT))
            .style(Style::default().bg(PANEL)),
    );
    frame.render_widget(status, layout[0]);
    let files = if app.workspace.files.is_empty() {
        vec![Line::from(Span::styled(
            "Clean working tree",
            Style::default().fg(MUTED),
        ))]
    } else {
        app.workspace
            .files
            .iter()
            .map(|file| {
                Line::from(vec![
                    Span::styled(format!("{} ", file.status), Style::default().fg(WARNING)),
                    Span::styled(file.path.clone(), Style::default().fg(TEXT)),
                ])
            })
            .collect()
    };
    let preview = Paragraph::new(files)
        .block(
            Block::default()
                .title(" FILES ")
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(PANEL_ALT))
                .style(Style::default().bg(PANEL)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(preview, layout[1]);

    let diff_lines = if app.workspace.diff_preview.is_empty() {
        vec![Line::from(Span::styled(
            "No staged or unstaged diff",
            Style::default().fg(MUTED),
        ))]
    } else {
        app.workspace
            .diff_preview
            .lines()
            .take(120)
            .map(|line| {
                let color = if line.starts_with('+') && !line.starts_with("+++") {
                    ACCENT
                } else if line.starts_with('-') && !line.starts_with("---") {
                    Color::Rgb(238, 126, 126)
                } else if line.starts_with("@@") {
                    BLUE
                } else {
                    MUTED
                };
                Line::from(Span::styled(line.to_string(), Style::default().fg(color)))
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(diff_lines)
            .block(
                Block::default()
                    .title(" DIFF PREVIEW ")
                    .borders(Borders::LEFT)
                    .border_style(Style::default().fg(PANEL_ALT))
                    .style(Style::default().bg(PANEL)),
            )
            .wrap(Wrap { trim: false }),
        layout[2],
    );
}

fn draw_status(app: &App, frame: &mut Frame, area: Rect) {
    let mode = match app.mode {
        Mode::Normal => "NORMAL",
        Mode::Compose => "INSERT",
    };
    let focus = match app.focus {
        Focus::Conversations => "conversations",
        Focus::Conversation => "conversation",
        Focus::Git => "git",
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {mode} "),
            Style::default()
                .fg(BG)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {focus}  "), Style::default().fg(TEXT)),
        Span::styled(
            "│  j/k scroll  i write  Tab panels  ? help  q quit",
            Style::default().fg(MUTED),
        ),
        Span::styled(
            format!("  {} ", app.status_message),
            Style::default().fg(WARNING),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(PANEL_ALT)),
        area,
    );
}

fn draw_overlay(frame: &mut Frame, area: Rect, overlay: &Overlay) {
    let width = area.width.min(72).saturating_sub(2);
    let height = match overlay {
        Overlay::Help => 15,
        Overlay::Palette { .. } => 9,
        Overlay::ProviderSelector => 8,
    };
    let popup = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);
    let (title, lines, selected) = match overlay {
        Overlay::Help => (
            " KEYBOARD ",
            vec![
                "j / k     navigate lists and scroll",
                "h / l     change focused panel",
                "i         write a message",
                "Esc       return to normal mode",
                "Tab       cycle focused panels",
                "Ctrl+p    command palette",
                "Ctrl+a    provider selector",
                "Ctrl+b    toggle project sidebar",
                "Ctrl+d    toggle workspace sidebar",
                "?         show this help",
                "q         quit in normal mode",
            ],
            None,
        ),
        Overlay::Palette { selected } => (
            " COMMAND PALETTE ",
            vec![
                "New conversation",
                "Toggle project sidebar",
                "Toggle workspace sidebar",
            ],
            Some(*selected),
        ),
        Overlay::ProviderSelector => (
            " PROVIDERS ",
            vec![
                "No providers configured.",
                "Provider integrations will appear here when configured.",
                "Esc       close",
            ],
            None,
        ),
    };
    let text = lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let style = if selected == Some(index) {
                Style::default().fg(BG).bg(ACCENT)
            } else {
                Style::default().fg(TEXT)
            };
            Line::from(Span::styled(
                if selected.is_some() {
                    format!("  {line}")
                } else {
                    line.to_string()
                },
                style,
            ))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(text)
            .block(
                Block::default()
                    .title(Span::styled(
                        title,
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT))
                    .style(Style::default().bg(PANEL)),
            )
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
