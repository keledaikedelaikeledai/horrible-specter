use std::{
    io::{self, stdout},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio::sync::mpsc;
use tokio::time::{self, MissedTickBehavior};

use crate::{
    persistence::{ConversationRecord, Database, MessageRecord, ProjectRecord},
    provider::ProviderRegistry,
    workspace::WorkspaceStatus,
};

const BG: Color = Color::Rgb(15, 18, 24);
const PANEL: Color = Color::Rgb(22, 27, 35);
const PANEL_ALT: Color = Color::Rgb(28, 34, 44);
const TEXT: Color = Color::Rgb(221, 226, 234);
const MUTED: Color = Color::Rgb(133, 145, 162);
const ACCENT: Color = Color::Rgb(93, 202, 177);
const BLUE: Color = Color::Rgb(116, 170, 255);
const WARNING: Color = Color::Rgb(238, 184, 92);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Conversations,
    Conversation,
    Git,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Compose,
}

pub struct App {
    project: ProjectRecord,
    database: Database,
    conversations: Vec<ConversationRecord>,
    active_conversation: usize,
    messages: Vec<MessageRecord>,
    workspace: WorkspaceStatus,
    providers: ProviderRegistry,
    focus: Focus,
    mode: Mode,
    composer: String,
    scroll_offset: usize,
    left_sidebar_visible: bool,
    right_sidebar_visible: bool,
    overlay: Option<Overlay>,
    status_message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    Help,
    Palette,
    ProviderSelector,
}

impl App {
    pub fn load(project_root: PathBuf, database: Database) -> Result<Self> {
        let project = database.ensure_project(&project_root)?;
        let mut conversations = database.list_conversations(project.id)?;
        if conversations.is_empty() {
            conversations.push(database.create_conversation(project.id, "New conversation")?);
        }
        let active_conversation = 0;
        let messages = database.list_messages(conversations[active_conversation].id)?;
        let workspace = WorkspaceStatus::read(&project_root)?;
        Ok(Self {
            project,
            database,
            conversations,
            active_conversation,
            messages,
            workspace,
            providers: ProviderRegistry::empty(),
            focus: Focus::Conversation,
            mode: Mode::Normal,
            composer: String::new(),
            scroll_offset: 0,
            left_sidebar_visible: true,
            right_sidebar_visible: true,
            overlay: None,
            status_message: "Ready. No provider configured; messages are saved locally."
                .to_string(),
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let backend = CrosstermBackend::new(stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        let result = self.event_loop(&mut terminal).await;
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), crossterm::cursor::Show, LeaveAlternateScreen);
        result
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let stop_reader = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop_reader);
        let reader = thread::spawn(move || {
            while !reader_stop.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(100)) {
                    Ok(true) => match event::read() {
                        Ok(input) => {
                            if event_sender.send(input).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        });
        let mut tick = time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let result = async {
            loop {
                terminal.draw(|frame| self.draw(frame))?;
                tokio::select! {
                    _ = tick.tick() => {
                        self.workspace = WorkspaceStatus::read(Path::new(&self.project.root_path))?;
                    }
                    Some(input) = event_receiver.recv() => {
                        match input {
                            Event::Key(key) if key.kind == KeyEventKind::Press => {
                                if self.handle_key(key)? { return Ok(()); }
                            }
                            Event::Resize(_, _) => {}
                            _ => {}
                        }
                    }
                    else => return Ok(()),
                }
            }
        }
        .await;
        stop_reader.store(true, Ordering::Relaxed);
        let _ = reader.join();
        result
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.overlay.is_some() {
            return self.handle_overlay_key(key);
        }
        if self.mode == Mode::Compose {
            return self.handle_compose_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('p') => self.overlay = Some(Overlay::Palette),
                KeyCode::Char('a') => self.overlay = Some(Overlay::ProviderSelector),
                KeyCode::Char('b') => self.left_sidebar_visible = !self.left_sidebar_visible,
                KeyCode::Char('d') => self.right_sidebar_visible = !self.right_sidebar_visible,
                _ => {}
            }
            return Ok(false);
        }
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Char('i') | KeyCode::Enter => {
                self.mode = Mode::Compose;
                self.focus = Focus::Conversation;
            }
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char('n') => self.new_conversation()?,
            KeyCode::Char('j') | KeyCode::Down => self.move_focus(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_focus(-1),
            KeyCode::Char('h') | KeyCode::Left => self.focus_previous(),
            KeyCode::Char('l') | KeyCode::Right => self.focus_next(),
            KeyCode::Tab => self.focus_next(),
            KeyCode::BackTab => self.focus_previous(),
            _ => {}
        }
        Ok(false)
    }

    fn handle_compose_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.submit_composer()?,
            KeyCode::Backspace => {
                self.composer.pop();
            }
            KeyCode::Char(character) => self.composer.push(character),
            _ => {}
        }
        Ok(false)
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Char('b') if self.overlay == Some(Overlay::Palette) => {
                self.left_sidebar_visible = !self.left_sidebar_visible;
                self.overlay = None;
            }
            KeyCode::Char('d') if self.overlay == Some(Overlay::Palette) => {
                self.right_sidebar_visible = !self.right_sidebar_visible;
                self.overlay = None;
            }
            KeyCode::Char('n') if self.overlay == Some(Overlay::Palette) => {
                self.new_conversation()?;
                self.overlay = None;
            }
            _ => {}
        }
        Ok(false)
    }

    fn submit_composer(&mut self) -> Result<()> {
        let content = self.composer.trim().to_string();
        if content.is_empty() {
            return Ok(());
        }
        let id = self.active_conversation_id();
        self.database.append_message(id, "user", &content)?;
        if self.conversations[self.active_conversation].title == "New conversation" {
            let title = content.chars().take(36).collect::<String>();
            self.database.rename_conversation(id, &title)?;
            self.conversations[self.active_conversation].title = title;
        }
        self.messages = self.database.list_messages(id)?;
        self.composer.clear();
        self.mode = Mode::Normal;
        self.scroll_offset = usize::MAX;
        self.status_message =
            "Saved locally. Configure a provider to continue this conversation.".to_string();
        Ok(())
    }

    fn new_conversation(&mut self) -> Result<()> {
        let conversation = self
            .database
            .create_conversation(self.project.id, "New conversation")?;
        self.conversations = self.database.list_conversations(self.project.id)?;
        self.active_conversation = self
            .conversations
            .iter()
            .position(|item| item.id == conversation.id)
            .unwrap_or(0);
        self.messages.clear();
        self.scroll_offset = 0;
        self.status_message = "New conversation created.".to_string();
        Ok(())
    }

    fn active_conversation_id(&self) -> i64 {
        self.conversations[self.active_conversation].id
    }

    fn move_focus(&mut self, direction: i32) {
        match self.focus {
            Focus::Conversations => {
                let max = self.conversations.len().saturating_sub(1);
                self.active_conversation = if direction > 0 {
                    (self.active_conversation + 1).min(max)
                } else {
                    self.active_conversation.saturating_sub(1)
                };
                self.messages = self
                    .database
                    .list_messages(self.active_conversation_id())
                    .unwrap_or_default();
                self.scroll_offset = 0;
            }
            Focus::Conversation => {
                if direction > 0 {
                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                }
            }
            Focus::Git => {}
        }
    }

    fn focus_next(&mut self) {
        self.focus = match self.focus {
            Focus::Conversations => Focus::Conversation,
            Focus::Conversation => Focus::Git,
            Focus::Git => Focus::Conversations,
        };
    }
    fn focus_previous(&mut self) {
        self.focus = match self.focus {
            Focus::Conversations => Focus::Git,
            Focus::Conversation => Focus::Conversations,
            Focus::Git => Focus::Conversation,
        };
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Block::default().style(Style::default().bg(BG)), area);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(2)])
            .split(area);
        let content = vertical[0];
        let status = vertical[1];
        let show_right = self.right_sidebar_visible && content.width >= 110;
        let show_left = self.left_sidebar_visible && content.width >= 72;
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
            self.draw_left(frame, columns[index]);
            index += 1;
        }
        self.draw_center(frame, columns[index]);
        index += 1;
        if show_right {
            self.draw_right(frame, columns[index]);
        }
        self.draw_status(frame, status);
        if let Some(overlay) = self.overlay {
            self.draw_overlay(frame, area, overlay);
        }
    }

    fn draw_left(&self, frame: &mut Frame, area: Rect) {
        let items = self
            .conversations
            .iter()
            .map(|conversation| {
                let marker = if conversation.id == self.active_conversation_id() {
                    "●"
                } else {
                    "○"
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{marker} "),
                        Style::default().fg(if marker == "●" { ACCENT } else { MUTED }),
                    ),
                    Span::styled(conversation.title.clone(), Style::default().fg(TEXT)),
                ]))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select((self.focus == Focus::Conversations).then_some(self.active_conversation));
        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(
                    " PROJECT ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(self.project.display_name.clone(), Style::default().fg(TEXT)),
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
                    format!("{} sessions", self.conversations.len()),
                    Style::default().fg(TEXT),
                )),
            ]),
            footer,
        );
    }

    fn draw_center(&self, frame: &mut Frame, area: Rect) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(4),
            ])
            .split(area);
        let conversation = &self.conversations[self.active_conversation];
        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    conversation.title.clone(),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  ·  ", Style::default().fg(MUTED)),
                Span::styled(self.providers.active_label(), Style::default().fg(WARNING)),
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
        if self.messages.is_empty() {
            lines.push(Line::from(Span::styled(
                "No messages yet",
                Style::default().fg(MUTED),
            )));
            lines.push(Line::from(Span::styled(
                "Press i to compose a message. Provider execution is not configured.",
                Style::default().fg(MUTED),
            )));
        } else {
            for message in &self.messages {
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
        let start = self.scroll_offset.min(max_scroll);
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

        let composer_title = if self.mode == Mode::Compose {
            " COMPOSER · INSERT "
        } else {
            " COMPOSER · PRESS i TO WRITE "
        };
        let composer = Paragraph::new(self.composer.as_str())
            .style(Style::default().fg(if self.mode == Mode::Compose {
                TEXT
            } else {
                MUTED
            }))
            .block(
                Block::default()
                    .title(Span::styled(
                        composer_title,
                        Style::default().fg(if self.mode == Mode::Compose {
                            ACCENT
                        } else {
                            MUTED
                        }),
                    ))
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(if self.mode == Mode::Compose {
                        ACCENT
                    } else {
                        PANEL_ALT
                    }))
                    .style(Style::default().bg(PANEL)),
            );
        frame.render_widget(composer, layout[2]);
    }

    fn draw_right(&self, frame: &mut Frame, area: Rect) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Length(7),
                Constraint::Min(4),
            ])
            .split(area);
        let branch = self
            .workspace
            .branch
            .as_deref()
            .unwrap_or("not a Git repository");
        let summary = if let Some(error) = &self.workspace.error {
            error.clone()
        } else if self.workspace.is_git_repo {
            format!("{} changed file(s)", self.workspace.files.len())
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
        let files = if self.workspace.files.is_empty() {
            vec![Line::from(Span::styled(
                "Clean working tree",
                Style::default().fg(MUTED),
            ))]
        } else {
            self.workspace
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

        let diff_lines = if self.workspace.diff_preview.is_empty() {
            vec![Line::from(Span::styled(
                "No unstaged diff",
                Style::default().fg(MUTED),
            ))]
        } else {
            self.workspace
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

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let mode = match self.mode {
            Mode::Normal => "NORMAL",
            Mode::Compose => "INSERT",
        };
        let focus = match self.focus {
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
                format!("  {} ", self.status_message),
                Style::default().fg(WARNING),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line).style(Style::default().bg(PANEL_ALT)),
            area,
        );
    }

    fn draw_overlay(&self, frame: &mut Frame, area: Rect, overlay: Overlay) {
        let width = area.width.min(72).saturating_sub(2);
        let height = match overlay {
            Overlay::Help => 15,
            Overlay::Palette => 9,
            Overlay::ProviderSelector => 8,
        };
        let popup = centered_rect(width, height, area);
        frame.render_widget(Clear, popup);
        let (title, lines) = match overlay {
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
            ),
            Overlay::Palette => (
                " COMMAND PALETTE ",
                vec![
                    "n         new conversation",
                    "b         toggle project sidebar",
                    "d         toggle workspace sidebar",
                    "Esc       close palette",
                ],
            ),
            Overlay::ProviderSelector => (
                " PROVIDERS ",
                vec![
                    "No providers configured.",
                    "Provider integrations will appear here when configured.",
                    "Esc       close",
                ],
            ),
        };
        let text = lines
            .into_iter()
            .map(|line| Line::from(Span::styled(line, Style::default().fg(TEXT))))
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
