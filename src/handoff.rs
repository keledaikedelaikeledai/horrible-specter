use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffCheckpoint {
    pub conversation_id: i64,
    pub from_provider: Option<String>,
    pub to_provider: Option<String>,
    pub source_provider_session_id: Option<String>,
    pub source_event_id: Option<i64>,
    pub context: HandoffContext,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HandoffContext {
    /// Application-owned history that remains available across providers.
    pub conversation_events: Vec<ContextEvent>,
    /// A provider-native session is supplemental and may not be transferable.
    pub native_provider_state: Option<NativeProviderState>,
    pub workspace_snapshot: Option<WorkspaceSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextEvent {
    pub role: String,
    pub content: String,
    pub sequence: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeProviderState {
    pub provider: String,
    pub session_id: String,
    pub state: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub root: String,
    pub branch: Option<String>,
    pub changed_files: Vec<String>,
}
