use std::{
    io::{self, stdout},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEvent, KeyEventKind},
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::{
    sync::mpsc,
    time::{self, MissedTickBehavior},
};

use crate::{
    input::{self, Action, Focus, Mode, Overlay},
    persistence::{ConversationRecord, Database, MessageRecord, ProjectRecord},
    provider::ProviderRegistry,
    ui,
    workspace::WorkspaceStatus,
};

pub struct App {
    pub(crate) project: ProjectRecord,
    pub(crate) database: Database,
    pub(crate) conversations: Vec<ConversationRecord>,
    pub(crate) active_conversation: usize,
    pub(crate) messages: Vec<MessageRecord>,
    pub(crate) workspace: WorkspaceStatus,
    pub(crate) providers: ProviderRegistry,
    pub(crate) focus: Focus,
    pub(crate) mode: Mode,
    pub(crate) composer: String,
    pub(crate) scroll_offset: usize,
    pub(crate) left_sidebar_visible: bool,
    pub(crate) right_sidebar_visible: bool,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) status_message: String,
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
        Ok(Self {
            project,
            database,
            conversations,
            active_conversation,
            messages,
            workspace: WorkspaceStatus::empty(project_root),
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

        let (refresh_request_tx, mut refresh_request_rx) = mpsc::channel::<()>(1);
        let (workspace_update_tx, mut workspace_update_rx) =
            mpsc::channel::<std::result::Result<WorkspaceStatus, String>>(1);
        let workspace_root = PathBuf::from(&self.project.root_path);
        let refresh_task = tokio::spawn(async move {
            while refresh_request_rx.recv().await.is_some() {
                while refresh_request_rx.try_recv().is_ok() {}
                let root = workspace_root.clone();
                let result =
                    tokio::task::spawn_blocking(move || WorkspaceStatus::read(&root)).await;
                let result = match result {
                    Ok(Ok(status)) => Ok(status),
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(error) => Err(format!("workspace refresh task failed: {error}")),
                };
                if workspace_update_tx.send(result).await.is_err() {
                    break;
                }
            }
        });
        request_refresh(&refresh_request_tx);

        let mut tick = time::interval(Duration::from_secs(3));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let result = async {
            loop {
                terminal.draw(|frame| ui::draw(self, frame))?;
                tokio::select! {
                    _ = tick.tick() => request_refresh(&refresh_request_tx),
                    Some(update) = workspace_update_rx.recv() => match update {
                        Ok(status) => self.workspace = status,
                        Err(error) => self.status_message = format!("Git refresh failed: {error}"),
                    },
                    Some(input) = event_receiver.recv() => {
                        if let Event::Key(key) = input
                            && key.kind == KeyEventKind::Press
                            && self.handle_key(key)?
                        {
                            return Ok(());
                        }
                    }
                    else => return Ok(()),
                }
            }
        }
        .await;

        stop_reader.store(true, Ordering::Relaxed);
        let _ = reader.join();
        refresh_task.abort();
        result
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        let action = if let Some(overlay) = &self.overlay {
            input::overlay_action(overlay, key)
        } else if self.mode == Mode::Compose {
            input::compose_action(key)
        } else {
            input::normal_action(key)
        };
        self.apply_action(action)
    }

    fn apply_action(&mut self, action: Action) -> Result<bool> {
        if self.overlay.is_some() {
            return self.apply_overlay_action(action);
        }
        if self.mode == Mode::Compose {
            return self.apply_compose_action(action);
        }
        match action {
            Action::Quit => Ok(true),
            Action::EnterCompose => {
                self.mode = Mode::Compose;
                self.focus = Focus::Conversation;
                Ok(false)
            }
            Action::OpenHelp => {
                self.overlay = Some(Overlay::Help);
                Ok(false)
            }
            Action::OpenPalette => {
                self.overlay = Some(Overlay::Palette { selected: 0 });
                Ok(false)
            }
            Action::OpenProviderSelector => {
                self.overlay = Some(Overlay::ProviderSelector);
                Ok(false)
            }
            Action::ToggleLeftSidebar => {
                self.left_sidebar_visible = !self.left_sidebar_visible;
                Ok(false)
            }
            Action::ToggleRightSidebar => {
                self.right_sidebar_visible = !self.right_sidebar_visible;
                Ok(false)
            }
            Action::NewConversation => {
                self.new_conversation()?;
                Ok(false)
            }
            Action::Move(direction) => {
                self.move_focus(direction)?;
                Ok(false)
            }
            Action::FocusNext => {
                self.focus = self.focus.next();
                Ok(false)
            }
            Action::FocusPrevious => {
                self.focus = self.focus.previous();
                Ok(false)
            }
            Action::None => Ok(false),
            _ => Ok(false),
        }
    }

    fn apply_compose_action(&mut self, action: Action) -> Result<bool> {
        match action {
            Action::ExitCompose => self.mode = Mode::Normal,
            Action::Submit => self.submit_composer()?,
            Action::InsertNewline => self.composer.push('\n'),
            Action::Backspace => {
                self.composer.pop();
            }
            Action::Insert(character) => self.composer.push(character),
            _ => {}
        }
        Ok(false)
    }

    fn apply_overlay_action(&mut self, action: Action) -> Result<bool> {
        match action {
            Action::CloseOverlay => self.overlay = None,
            Action::MoveOverlay(direction) => {
                if let Some(Overlay::Palette { selected }) = &mut self.overlay {
                    *selected = if direction > 0 {
                        (*selected + 1).min(Overlay::PALETTE_LENGTH - 1)
                    } else {
                        selected.saturating_sub(1)
                    };
                }
            }
            Action::ActivateOverlay => {
                let selected = self
                    .overlay
                    .as_ref()
                    .and_then(Overlay::selected_palette_command);
                self.overlay = None;
                match selected {
                    Some(0) => self.new_conversation()?,
                    Some(1) => self.left_sidebar_visible = !self.left_sidebar_visible,
                    Some(2) => self.right_sidebar_visible = !self.right_sidebar_visible,
                    _ => {}
                }
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
        let title = (self.conversations[self.active_conversation].title == "New conversation")
            .then(|| {
                content
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(36)
                    .collect::<String>()
            });
        self.database
            .append_message(id, "user", &content, title.as_deref())?;
        if let Some(title) = title {
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

    pub(crate) fn active_conversation_id(&self) -> i64 {
        self.conversations[self.active_conversation].id
    }

    fn move_focus(&mut self, direction: i8) -> Result<()> {
        match self.focus {
            Focus::Conversations => {
                let max = self.conversations.len().saturating_sub(1);
                self.active_conversation = if direction > 0 {
                    (self.active_conversation + 1).min(max)
                } else {
                    self.active_conversation.saturating_sub(1)
                };
                self.messages = self.database.list_messages(self.active_conversation_id())?;
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
        Ok(())
    }
}

fn request_refresh(sender: &mpsc::Sender<()>) {
    let _ = sender.try_send(());
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    #[test]
    fn multiline_composition_persists_as_one_user_message() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        fs::create_dir_all(&root)?;
        let database = Database::open(&root)?;
        let mut app = App::load(PathBuf::from(&root), database)?;

        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))?;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE))?;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))?;
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE))?;
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].content, "a\nb");
        Ok(())
    }
}
