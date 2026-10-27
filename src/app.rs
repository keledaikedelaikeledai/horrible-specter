use std::{
    io::{self, stdout},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
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
    persistence::{ConversationRecord, Database, EventInput, MessageRecord, ProjectRecord},
    provider::{
        ApplicationContext, ContextMessage, MessageRole, OperationStatus, ProviderError,
        ProviderEvent, ProviderEventKind, ProviderRegistry, ProviderRequest, ProviderSession,
        SessionRequest,
    },
    ui,
    workspace::WorkspaceStatus,
};

static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
enum ProviderRuntimeMessage {
    RunReady {
        run_id: String,
        session: ProviderSession,
    },
    Event(ProviderEvent),
    RunEnded {
        run_id: String,
        status: String,
    },
    StartFailed {
        run_id: String,
        error: ProviderError,
    },
}

#[derive(Clone, Debug)]
struct ActiveRun {
    run_id: String,
    conversation_id: i64,
    provider_session_id: Option<String>,
    persistence_failed: bool,
}

pub struct App {
    pub(crate) project: ProjectRecord,
    pub(crate) database: Database,
    pub(crate) conversations: Vec<ConversationRecord>,
    pub(crate) active_conversation: usize,
    pub(crate) messages: Vec<MessageRecord>,
    pub(crate) workspace: WorkspaceStatus,
    pub(crate) providers: ProviderRegistry,
    pub(crate) opencode: Option<crate::opencode::OpenCodeClient>,
    pub(crate) focus: Focus,
    pub(crate) mode: Mode,
    pub(crate) composer: String,
    pub(crate) scroll_offset: usize,
    pub(crate) left_sidebar_visible: bool,
    pub(crate) right_sidebar_visible: bool,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) status_message: String,
    pub(crate) activities: Vec<String>,
    active_run: Option<ActiveRun>,
    runtime_sender: Option<mpsc::UnboundedSender<ProviderRuntimeMessage>>,
}

impl App {
    pub async fn discover_opencode(&mut self) {
        match crate::opencode::OpenCodeClient::local() {
            Ok(client) => {
                self.opencode = Some(client.clone());
                let health = time::timeout(Duration::from_secs(3), client.health()).await;
                match health {
                    Ok(Ok(version)) => {
                        let providers =
                            time::timeout(Duration::from_secs(3), client.providers()).await;
                        match providers {
                            Ok(Ok(descriptors)) if !descriptors.is_empty() => {
                                self.providers = ProviderRegistry::from_descriptors(descriptors);
                                self.status_message =
                                    format!("OpenCode {version} connected; model available");
                            }
                            Ok(Ok(_)) => {
                                self.status_message =
                                    format!("OpenCode {version} connected; no models configured")
                            }
                            Ok(Err(error)) => {
                                self.status_message =
                                    format!("OpenCode provider discovery failed: {error}")
                            }
                            Err(_) => {
                                self.status_message =
                                    "OpenCode provider discovery timed out".to_string()
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        self.status_message = format!("OpenCode connection failed: {error}")
                    }
                    Err(_) => self.status_message = "OpenCode health check timed out".to_string(),
                }
            }
            Err(error) => self.status_message = format!("OpenCode setup failed: {error}"),
        }
    }
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
        let activities =
            activity_lines(&database.list_events(conversations[active_conversation].id)?);
        Ok(Self {
            project,
            database,
            conversations,
            active_conversation,
            messages,
            workspace: WorkspaceStatus::empty(project_root),
            providers: ProviderRegistry::empty(),
            opencode: None,
            focus: Focus::Conversation,
            mode: Mode::Normal,
            composer: String::new(),
            scroll_offset: 0,
            left_sidebar_visible: true,
            right_sidebar_visible: true,
            overlay: None,
            status_message: "Ready. No provider configured; messages are saved locally."
                .to_string(),
            activities,
            active_run: None,
            runtime_sender: None,
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
        let (provider_sender, mut provider_receiver) = mpsc::unbounded_channel();
        self.runtime_sender = Some(provider_sender);
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
                    Some(provider_message) = provider_receiver.recv() => {
                        if let Err(error) = self.handle_provider_message(provider_message).await {
                            if let Some(run) = &mut self.active_run {
                                run.persistence_failed = true;
                            }
                            self.status_message = format!("Provider persistence failed: {error}");
                        }
                    }
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

        self.shutdown_active_run().await;
        stop_reader.store(true, Ordering::Relaxed);
        let _ = reader.join();
        refresh_task.abort();
        self.runtime_sender = None;
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
                self.overlay = Some(Overlay::ProviderSelector {
                    selected: self.providers.active_index(),
                });
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
            Action::MoveOverlay(direction) => match &mut self.overlay {
                Some(Overlay::Palette { selected }) => {
                    *selected = if direction > 0 {
                        (*selected + 1).min(Overlay::PALETTE_LENGTH - 1)
                    } else {
                        selected.saturating_sub(1)
                    };
                }
                Some(Overlay::ProviderSelector { selected }) => {
                    let max = self.providers.descriptors().len().saturating_sub(1);
                    *selected = if direction > 0 {
                        (*selected + 1).min(max)
                    } else {
                        selected.saturating_sub(1)
                    };
                }
                _ => {}
            },
            Action::ActivateOverlay => {
                let selected = self
                    .overlay
                    .as_ref()
                    .and_then(Overlay::selected_palette_command);
                let selected_provider = self.overlay.as_ref().and_then(|overlay| match overlay {
                    Overlay::ProviderSelector { selected } => Some(*selected),
                    _ => None,
                });
                self.overlay = None;
                match selected {
                    Some(0) => self.new_conversation()?,
                    Some(1) => self.left_sidebar_visible = !self.left_sidebar_visible,
                    Some(2) => self.right_sidebar_visible = !self.right_sidebar_visible,
                    _ => {}
                }
                if let Some(selected) = selected_provider {
                    self.providers.select(selected);
                    self.status_message = format!(
                        "Selected {}.",
                        self.providers
                            .active_model()
                            .unwrap_or(self.providers.active_label())
                    );
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
        if self.active_run.is_some() {
            self.status_message =
                "A provider run is still active; wait for it to stop.".to_string();
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
        if self.opencode.is_some() && self.providers.active_descriptor().is_some() {
            if let Some(sender) = self.runtime_sender.clone()
                && let Err(error) = self.start_provider_run(sender, content)
            {
                self.status_message = format!("OpenCode could not start: {error}");
            }
        } else {
            self.status_message =
                "Saved locally. Connect OpenCode to continue this conversation.".to_string();
        }
        Ok(())
    }

    fn new_conversation(&mut self) -> Result<()> {
        if self.active_run.is_some() {
            self.status_message =
                "Finish the active provider run before changing conversations.".to_string();
            return Ok(());
        }
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
        self.activities.clear();
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
                if self.active_run.is_some() {
                    self.status_message =
                        "Finish the active provider run before changing conversations.".to_string();
                    return Ok(());
                }
                let max = self.conversations.len().saturating_sub(1);
                self.active_conversation = if direction > 0 {
                    (self.active_conversation + 1).min(max)
                } else {
                    self.active_conversation.saturating_sub(1)
                };
                self.messages = self.database.list_messages(self.active_conversation_id())?;
                self.activities =
                    activity_lines(&self.database.list_events(self.active_conversation_id())?);
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

    fn start_provider_run(
        &mut self,
        sender: mpsc::UnboundedSender<ProviderRuntimeMessage>,
        prompt: String,
    ) -> Result<()> {
        let descriptor = self
            .providers
            .active_descriptor()
            .cloned()
            .ok_or_else(|| anyhow!("no OpenCode model is selected"))?;
        let client = self
            .opencode
            .clone()
            .ok_or_else(|| anyhow!("OpenCode is not connected"))?;
        let conversation_id = self.active_conversation_id();
        let native_session_id = self
            .database
            .latest_provider_session(conversation_id, &descriptor.id)?
            .and_then(|session| session.native_session_id);
        let events = self.database.list_events(conversation_id)?;
        let request = ProviderRequest {
            conversation_id,
            prompt,
            model: descriptor
                .model
                .as_ref()
                .map(|model| format!("{}/{}", descriptor.id, model)),
            session: native_session_id
                .clone()
                .map(|native_session_id| SessionRequest::Resume { native_session_id })
                .unwrap_or(SessionRequest::New),
            run_id: format!(
                "run-{}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed)
            ),
            application_context: ApplicationContext {
                messages: self
                    .messages
                    .iter()
                    .map(|message| ContextMessage {
                        role: match message.role.as_str() {
                            "user" => MessageRole::User,
                            "assistant" => MessageRole::Assistant,
                            _ => MessageRole::System,
                        },
                        content: message.content.clone(),
                        sequence: message.id,
                    })
                    .collect(),
                last_event_sequence: events.last().map(|event| event.sequence),
            },
        };
        self.database.start_provider_run(
            self.project.id,
            conversation_id,
            &request.run_id,
            &descriptor.id,
            native_session_id.as_deref(),
        )?;
        self.active_run = Some(ActiveRun {
            run_id: request.run_id.clone(),
            conversation_id,
            provider_session_id: None,
            persistence_failed: false,
        });
        self.status_message = format!(
            "Running OpenCode with {}...",
            descriptor.model.clone().unwrap_or_default()
        );
        let run_id = request.run_id.clone();
        tokio::spawn(async move {
            match client.start_run(request).await {
                Ok(mut run) => {
                    let _ = sender.send(ProviderRuntimeMessage::RunReady {
                        run_id: run.run_id.clone(),
                        session: run.session.clone(),
                    });
                    let mut status = "uncertain".to_string();
                    while let Some(event) = run.events.recv().await {
                        status = provider_event_status(&event.kind).to_string();
                        let terminal = is_terminal_event(&event.kind);
                        if sender.send(ProviderRuntimeMessage::Event(event)).is_err() {
                            return;
                        }
                        if terminal {
                            break;
                        }
                    }
                    let _ = sender.send(ProviderRuntimeMessage::RunEnded { run_id, status });
                }
                Err(error) => {
                    let _ = sender.send(ProviderRuntimeMessage::StartFailed {
                        run_id,
                        error: crate::opencode::classify_error(error),
                    });
                }
            }
        });
        Ok(())
    }

    async fn handle_provider_message(&mut self, message: ProviderRuntimeMessage) -> Result<()> {
        match message {
            ProviderRuntimeMessage::RunReady { run_id, session } => {
                if self
                    .active_run
                    .as_ref()
                    .is_some_and(|run| run.run_id == run_id)
                {
                    if let Some(run) = &mut self.active_run {
                        run.provider_session_id = Some(session.provider_session_id.clone());
                    }
                    self.database
                        .set_provider_session_native_id(&run_id, &session.provider_session_id)?;
                    self.status_message =
                        "OpenCode session connected; streaming response...".to_string();
                }
            }
            ProviderRuntimeMessage::Event(event) => self.persist_provider_event(event).await?,
            ProviderRuntimeMessage::RunEnded { run_id, status } => {
                if self
                    .active_run
                    .as_ref()
                    .is_some_and(|run| run.run_id == run_id)
                {
                    let persistence_failed = self
                        .active_run
                        .as_ref()
                        .is_some_and(|run| run.persistence_failed);
                    if !persistence_failed {
                        self.database
                            .finish_provider_run(self.project.id, &run_id, &status)?;
                        self.active_run = None;
                        self.status_message = if status == "uncertain" {
                            "OpenCode disconnected; partial output was saved.".to_string()
                        } else {
                            format!("OpenCode run {status}.")
                        };
                    } else {
                        self.status_message =
                            "OpenCode stopped, but persistence failed; lease retained for recovery."
                                .to_string();
                    }
                }
            }
            ProviderRuntimeMessage::StartFailed { run_id, error } => {
                if let Some(run) = self.active_run.clone()
                    && run.run_id == run_id
                {
                    let message = error.to_string();
                    let scope = format!("{run_id}:startup");
                    let metadata = serde_json::to_string(&error).ok();
                    self.database.append_event(
                        run.conversation_id,
                        EventInput {
                            kind: "provider.failed",
                            role: Some("system"),
                            content: Some(&message),
                            event_key: Some("startup"),
                            event_scope: Some(&scope),
                            provider_session_id: None,
                            tool_call_id: None,
                            message_id: None,
                            operation_status: Some("failed"),
                            metadata_json: metadata.as_deref(),
                            conversation_title: None,
                        },
                    )?;
                    self.database
                        .finish_provider_run(self.project.id, &run_id, "failed")?;
                    self.active_run = None;
                    self.reload_conversation_state(run.conversation_id)?;
                    self.status_message = format!("OpenCode failed: {message}");
                }
            }
        }
        Ok(())
    }

    async fn persist_provider_event(&mut self, event: ProviderEvent) -> Result<()> {
        let Some(run) = self.active_run.clone() else {
            return Ok(());
        };
        if run.run_id != event.run_id {
            return Ok(());
        }
        let (kind, role, content, operation_status) = normalize_provider_event(&event.kind);
        let scope = event.event_scope();
        let metadata = serde_json::to_string(&event.kind).ok();
        let project_root = PathBuf::from(&self.project.root_path);
        let project_id = self.project.id;
        let event_id = event.event_id.clone();
        let provider_run_id = event.run_id.clone();
        let provider_session_id = event.provider_session_id.clone();
        let tool_call_id = event.tool_call_id.clone();
        let message_id = event.message_id.clone();
        let terminal_status =
            is_terminal_event(&event.kind).then(|| provider_event_status(&event.kind));
        let (messages, activities) = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut database = Database::open(&project_root)?;
            database.append_event(
                run.conversation_id,
                EventInput {
                    kind,
                    role,
                    content: content.as_deref(),
                    event_key: Some(&event_id),
                    event_scope: Some(&scope),
                    provider_session_id: Some(&provider_session_id),
                    tool_call_id: tool_call_id.as_deref(),
                    message_id: message_id.as_deref(),
                    operation_status,
                    metadata_json: metadata.as_deref(),
                    conversation_title: None,
                },
            )?;
            if let Some(status) = terminal_status {
                database.finish_provider_run(project_id, &provider_run_id, status)?;
            }
            let messages = database.list_messages(run.conversation_id)?;
            let activities = activity_lines(&database.list_events(run.conversation_id)?);
            Ok((messages, activities))
        })
        .await??;
        if run.conversation_id == self.active_conversation_id() {
            self.messages = messages;
            self.activities = activities;
            self.scroll_offset = usize::MAX;
        }
        self.status_message = match &event.kind {
            ProviderEventKind::Completed => "OpenCode session idle.".to_string(),
            ProviderEventKind::Failed { error } => format!("OpenCode failed: {error}"),
            ProviderEventKind::ToolStarted { name, .. } => format!("OpenCode running {name}..."),
            ProviderEventKind::ToolOutput { .. } => "OpenCode tool output received.".to_string(),
            _ => "OpenCode is streaming...".to_string(),
        };
        if is_terminal_event(&event.kind) {
            self.active_run = None;
        }
        Ok(())
    }

    fn reload_conversation_state(&mut self, conversation_id: i64) -> Result<()> {
        if conversation_id == self.active_conversation_id() {
            self.messages = self.database.list_messages(conversation_id)?;
            self.activities = activity_lines(&self.database.list_events(conversation_id)?);
            self.scroll_offset = usize::MAX;
        }
        Ok(())
    }

    async fn shutdown_active_run(&mut self) {
        let Some(run) = self.active_run.clone() else {
            return;
        };
        let Some(session_id) = run.provider_session_id else {
            return;
        };
        let Some(client) = self.opencode.clone() else {
            return;
        };
        match client.abort_session(&session_id).await {
            Ok(true) => {
                let scope = format!("{}:{session_id}", run.run_id);
                let content = "OpenCode run interrupted during Serangkai shutdown";
                let append = self.database.append_event(
                    run.conversation_id,
                    EventInput {
                        kind: "provider.interrupted",
                        role: Some("system"),
                        content: Some(content),
                        event_key: Some("shutdown"),
                        event_scope: Some(&scope),
                        provider_session_id: Some(&session_id),
                        tool_call_id: None,
                        message_id: None,
                        operation_status: Some("interrupted"),
                        metadata_json: None,
                        conversation_title: None,
                    },
                );
                if append.is_ok()
                    && self
                        .database
                        .finish_provider_run(self.project.id, &run.run_id, "interrupted")
                        .is_ok()
                {
                    self.active_run = None;
                }
            }
            Ok(false) => {}
            Err(_) => {}
        }
    }
}

fn request_refresh(sender: &mpsc::Sender<()>) {
    let _ = sender.try_send(());
}

fn normalize_provider_event(
    event: &ProviderEventKind,
) -> (
    &'static str,
    Option<&'static str>,
    Option<String>,
    Option<&'static str>,
) {
    match event {
        ProviderEventKind::SessionStarted => (
            "provider.session.started",
            Some("system"),
            Some("OpenCode session started".to_string()),
            Some("running"),
        ),
        ProviderEventKind::MessageDelta { role, content } => (
            "message.delta",
            Some(message_role_name(role)),
            Some(content.clone()),
            Some("running"),
        ),
        ProviderEventKind::MessageCompleted { role, content } => (
            "message.completed",
            Some(message_role_name(role)),
            Some(content.clone()),
            Some("completed"),
        ),
        ProviderEventKind::ToolStarted { name, input, .. } => (
            "tool.started",
            Some("system"),
            Some(format!("{name}: {input}")),
            Some("running"),
        ),
        ProviderEventKind::ToolOutput {
            output, is_final, ..
        } => (
            "tool.output",
            Some("system"),
            Some(output.clone()),
            Some(if *is_final { "completed" } else { "partial" }),
        ),
        ProviderEventKind::ToolFinished { status, error, .. } => (
            "tool.finished",
            Some("system"),
            error.clone(),
            Some(operation_status_name(status)),
        ),
        ProviderEventKind::Completed => (
            "provider.completed",
            Some("system"),
            Some("OpenCode session idle".to_string()),
            Some("completed"),
        ),
        ProviderEventKind::Failed { error } => (
            "provider.failed",
            Some("system"),
            Some(error.to_string()),
            Some("failed"),
        ),
        ProviderEventKind::Interrupted => (
            "provider.interrupted",
            Some("system"),
            Some("OpenCode run interrupted".to_string()),
            Some("interrupted"),
        ),
        ProviderEventKind::Notice { message } => (
            "provider.notice",
            Some("system"),
            Some(message.clone()),
            None,
        ),
    }
}

fn message_role_name(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
    }
}

fn operation_status_name(status: &OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "running",
        OperationStatus::Completed => "completed",
        OperationStatus::Failed => "failed",
        OperationStatus::Interrupted => "interrupted",
        OperationStatus::Partial => "partial",
        OperationStatus::Uncertain => "uncertain",
    }
}

fn is_terminal_event(event: &ProviderEventKind) -> bool {
    matches!(
        event,
        ProviderEventKind::Completed
            | ProviderEventKind::Failed { .. }
            | ProviderEventKind::Interrupted
    )
}

fn provider_event_status(event: &ProviderEventKind) -> &'static str {
    match event {
        ProviderEventKind::Completed => "completed",
        ProviderEventKind::Failed { .. } => "failed",
        ProviderEventKind::Interrupted => "interrupted",
        _ => "uncertain",
    }
}

fn activity_lines(events: &[crate::persistence::EventRecord]) -> Vec<String> {
    events
        .iter()
        .filter(|event| {
            event.kind.starts_with("tool.")
                || event.kind.starts_with("provider.")
                || event.kind == "notice"
        })
        .filter_map(|event| {
            event.content.as_ref().map(|content| {
                let content = content.chars().take(160).collect::<String>();
                format!("{}: {content}", event.kind)
            })
        })
        .collect()
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

    #[tokio::test]
    async fn provider_events_persist_incrementally_and_replay_after_restart() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        std::fs::create_dir_all(&root)?;
        let database = Database::open(&root)?;
        let mut app = App::load(root.clone(), database)?;
        let conversation_id = app.active_conversation_id();
        app.database.start_provider_run(
            app.project.id,
            conversation_id,
            "run-1",
            "fixture",
            Some("native-1"),
        )?;
        app.active_run = Some(ActiveRun {
            run_id: "run-1".to_string(),
            conversation_id,
            provider_session_id: Some("native-1".to_string()),
            persistence_failed: false,
        });
        app.persist_provider_event(ProviderEvent {
            event_id: "delta-1".to_string(),
            provider_session_id: "native-1".to_string(),
            run_id: "run-1".to_string(),
            message_id: Some("message-1".to_string()),
            tool_call_id: None,
            kind: ProviderEventKind::MessageDelta {
                role: MessageRole::Assistant,
                content: "hello".to_string(),
            },
        })
        .await?;
        assert_eq!(app.messages[0].content, "hello");
        app.persist_provider_event(ProviderEvent {
            event_id: "tool-1".to_string(),
            provider_session_id: "native-1".to_string(),
            run_id: "run-1".to_string(),
            message_id: Some("message-1".to_string()),
            tool_call_id: Some("call-1".to_string()),
            kind: ProviderEventKind::ToolStarted {
                call_id: "call-1".to_string(),
                name: "bash".to_string(),
                input: "pwd".to_string(),
            },
        })
        .await?;
        assert!(
            app.activities
                .iter()
                .any(|activity| activity.contains("tool.started"))
        );
        app.persist_provider_event(ProviderEvent {
            event_id: "idle-1".to_string(),
            provider_session_id: "native-1".to_string(),
            run_id: "run-1".to_string(),
            message_id: None,
            tool_call_id: None,
            kind: ProviderEventKind::Completed,
        })
        .await?;
        assert!(app.active_run.is_none());
        drop(app);

        let reopened = Database::open(&root)?;
        assert_eq!(reopened.list_messages(conversation_id)?[0].content, "hello");
        assert_eq!(
            reopened
                .latest_provider_session(conversation_id, "fixture")?
                .expect("provider session")
                .native_session_id
                .as_deref(),
            Some("native-1")
        );
        Ok(())
    }
}
