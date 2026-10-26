use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    future::Future,
    pin::Pin,
};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub type ProviderResult<T> = std::result::Result<T, ProviderError>;
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = ProviderResult<T>> + Send + 'a>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub streaming_messages: bool,
    pub tool_events: bool,
    pub interruption: bool,
    pub native_session_resume: bool,
    pub usage_limits: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub model: Option<String>,
    pub capabilities: ProviderCapabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub conversation_id: i64,
    pub prompt: String,
    pub model: Option<String>,
    pub session: SessionRequest,
    pub run_id: String,
    pub application_context: ApplicationContext,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionRequest {
    New,
    Resume { native_session_id: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ApplicationContext {
    /// Application-owned history remains the source of truth for handoff.
    pub messages: Vec<ContextMessage>,
    pub last_event_sequence: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextMessage {
    pub role: MessageRole,
    pub content: String,
    pub sequence: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSession {
    pub provider_session_id: String,
    pub resumable: bool,
}

#[derive(Debug)]
pub struct ProviderRun {
    pub run_id: String,
    pub session: ProviderSession,
    pub events: mpsc::Receiver<ProviderEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderEvent {
    /// Stable provider event identity used for idempotent persistence.
    pub event_id: String,
    pub provider_session_id: String,
    pub run_id: String,
    pub message_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub kind: ProviderEventKind,
}

impl ProviderEvent {
    pub fn event_scope(&self) -> String {
        format!("{}:{}", self.run_id, self.provider_session_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProviderEventKind {
    SessionStarted,
    MessageDelta {
        role: MessageRole,
        content: String,
    },
    MessageCompleted {
        role: MessageRole,
        content: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
        input: String,
    },
    /// A provider may emit several output events before ToolFinished.
    ToolOutput {
        call_id: String,
        output: String,
        is_final: bool,
    },
    ToolFinished {
        call_id: String,
        status: OperationStatus,
        error: Option<String>,
    },
    Completed,
    Failed {
        error: ProviderError,
    },
    Interrupted,
    Notice {
        message: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OperationStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
    Partial,
    Uncertain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProviderError {
    Authentication {
        message: String,
    },
    UsageLimit {
        message: String,
        retry_after_seconds: Option<u64>,
    },
    Unsupported {
        message: String,
    },
    Interrupted {
        message: String,
    },
    Transport {
        message: String,
    },
    Protocol {
        message: String,
    },
    Failed {
        message: String,
    },
}

impl Display for ProviderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication { message }
            | Self::UsageLimit { message, .. }
            | Self::Unsupported { message }
            | Self::Interrupted { message }
            | Self::Transport { message }
            | Self::Protocol { message }
            | Self::Failed { message } => formatter.write_str(message),
        }
    }
}

impl Error for ProviderError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterruptResult {
    pub provider_acknowledged: bool,
    pub final_status: OperationStatus,
}

/// A common boundary for provider integrations. Implementations own their native
/// protocol details while exposing observable, replayable events to the app.
pub trait Provider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;
    fn start<'a>(&'a self, request: ProviderRequest) -> ProviderFuture<'a, ProviderRun>;
    fn interrupt<'a>(&'a self, session: &'a ProviderSession)
    -> ProviderFuture<'a, InterruptResult>;
}

#[derive(Clone, Debug, Default)]
pub struct ProviderRegistry {
    descriptors: Vec<ProviderDescriptor>,
}

impl ProviderRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn descriptors(&self) -> &[ProviderDescriptor] {
        &self.descriptors
    }

    pub fn active_label(&self) -> &str {
        self.descriptors
            .first()
            .map(|provider| provider.display_name.as_str())
            .unwrap_or("No provider configured")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_limits_and_partial_tool_states_are_representable() {
        let error = ProviderError::UsageLimit {
            message: "monthly limit reached".to_string(),
            retry_after_seconds: Some(3600),
        };
        let event = ProviderEvent {
            event_id: "tool-1-finish".to_string(),
            provider_session_id: "session-1".to_string(),
            run_id: "run-1".to_string(),
            message_id: None,
            tool_call_id: Some("call-1".to_string()),
            kind: ProviderEventKind::ToolFinished {
                call_id: "call-1".to_string(),
                status: OperationStatus::Partial,
                error: Some(error.to_string()),
            },
        };
        assert!(matches!(error, ProviderError::UsageLimit { .. }));
        assert!(matches!(
            event.kind,
            ProviderEventKind::ToolFinished {
                status: OperationStatus::Partial,
                ..
            }
        ));
    }
}
