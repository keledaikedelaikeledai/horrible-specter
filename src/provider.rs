use std::{future::Future, pin::Pin};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub streaming_messages: bool,
    pub tool_events: bool,
    pub interruption: bool,
    pub native_session_resume: bool,
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
}

#[derive(Debug)]
pub struct ProviderRun {
    pub provider_session_id: String,
    pub events: mpsc::Receiver<ProviderEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProviderEvent {
    SessionStarted {
        provider_session_id: String,
    },
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
    ToolOutput {
        call_id: String,
        output: String,
        status: OperationStatus,
    },
    Completed,
    Failed {
        message: String,
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
    Completed,
    Failed,
    Interrupted,
    Uncertain,
}

/// A common boundary for provider integrations. Implementations own their native
/// protocol details while exposing observable events to the application.
pub trait Provider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;
    fn start<'a>(&'a self, request: ProviderRequest) -> ProviderFuture<'a, ProviderRun>;
    fn interrupt<'a>(&'a self, provider_session_id: &'a str) -> ProviderFuture<'a, ()>;
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
