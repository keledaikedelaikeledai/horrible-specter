use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::handoff::{ContextEvent, HandoffContext};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

#[derive(Clone, Debug)]
pub struct ProjectRecord {
    pub id: i64,
    pub root_path: String,
    pub display_name: String,
}

#[derive(Clone, Debug)]
pub struct ConversationRecord {
    pub id: i64,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct MessageRecord {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct EventRecord {
    pub id: i64,
    pub conversation_id: i64,
    pub sequence: i64,
    pub kind: String,
    pub role: Option<String>,
    pub content: Option<String>,
    pub event_key: Option<String>,
    pub event_scope: String,
    pub provider_session_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub message_id: Option<String>,
    pub operation_status: Option<String>,
    pub metadata_json: Option<String>,
    pub created_at: i64,
}

#[derive(Clone, Copy)]
pub struct EventInput<'a> {
    pub kind: &'a str,
    pub role: Option<&'a str>,
    pub content: Option<&'a str>,
    pub event_key: Option<&'a str>,
    pub event_scope: Option<&'a str>,
    pub provider_session_id: Option<&'a str>,
    pub tool_call_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub operation_status: Option<&'a str>,
    pub metadata_json: Option<&'a str>,
    pub conversation_title: Option<&'a str>,
}

pub struct Database {
    connection: Connection,
    pub path: PathBuf,
}

impl Database {
    pub fn open(project_root: &Path) -> Result<Self> {
        let data_dir = project_root.join(".serangkai");
        fs::create_dir_all(&data_dir)?;
        let path = data_dir.join("serangkai.db");
        let connection = Connection::open(&path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let database = Self { connection, path };
        database.migrate()?;
        Ok(database)
    }

    fn migrate(&self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );",
        )?;
        let mut applied = self
            .connection
            .query_row(
                "SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let mut version = applied.take().unwrap_or(0);
        if version < 1 {
            let tx = self.connection.unchecked_transaction()?;
            tx.execute_batch(
                "CREATE TABLE projects (
                    id INTEGER PRIMARY KEY,
                    root_path TEXT NOT NULL UNIQUE,
                    display_name TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE conversations (
                    id INTEGER PRIMARY KEY,
                    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    title TEXT NOT NULL,
                    provider_name TEXT,
                    provider_model TEXT,
                    status TEXT NOT NULL DEFAULT 'active',
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE events (
                    id INTEGER PRIMARY KEY,
                    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    role TEXT,
                    content TEXT,
                    provider_session_id TEXT,
                    operation_status TEXT,
                    metadata_json TEXT,
                    created_at INTEGER NOT NULL,
                    UNIQUE(conversation_id, sequence)
                );
                CREATE TABLE provider_sessions (
                    id INTEGER PRIMARY KEY,
                    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                    provider TEXT NOT NULL,
                    native_session_id TEXT,
                    status TEXT NOT NULL,
                    started_at INTEGER NOT NULL,
                    ended_at INTEGER,
                    metadata_json TEXT
                );
                CREATE TABLE handoff_checkpoints (
                    id INTEGER PRIMARY KEY,
                    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                    from_provider TEXT,
                    to_provider TEXT,
                    source_provider_session_id TEXT,
                    source_event_id INTEGER,
                    context_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX conversations_project_updated ON conversations(project_id, updated_at DESC);
                CREATE INDEX events_conversation_sequence ON events(conversation_id, sequence);
                CREATE INDEX provider_sessions_conversation ON provider_sessions(conversation_id, started_at);
                INSERT INTO schema_migrations(version, applied_at) VALUES (1, strftime('%s', 'now'));",
            )?;
            tx.commit()?;
            version = 1;
        }
        if version < 2 {
            let tx = self.connection.unchecked_transaction()?;
            tx.execute_batch(
                "ALTER TABLE events ADD COLUMN event_key TEXT;
                 CREATE UNIQUE INDEX events_conversation_event_key
                   ON events(conversation_id, event_key)
                   WHERE event_key IS NOT NULL;
                 INSERT INTO schema_migrations(version, applied_at)
                   VALUES (2, strftime('%s', 'now'));",
            )?;
            tx.commit()?;
            version = 2;
        }
        if version < 3 {
            let tx = self.connection.unchecked_transaction()?;
            tx.execute_batch(
                "ALTER TABLE events ADD COLUMN event_scope TEXT NOT NULL DEFAULT 'application';
                 ALTER TABLE events ADD COLUMN tool_call_id TEXT;
                 ALTER TABLE events ADD COLUMN message_id TEXT;
                 ALTER TABLE provider_sessions ADD COLUMN run_id TEXT NOT NULL DEFAULT '';
                 DROP INDEX IF EXISTS events_conversation_event_key;
                 CREATE UNIQUE INDEX events_conversation_scoped_event_key
                   ON events(conversation_id, event_scope, event_key)
                   WHERE event_key IS NOT NULL;
                 CREATE UNIQUE INDEX provider_sessions_run_id
                   ON provider_sessions(run_id)
                   WHERE run_id <> '';
                 CREATE TABLE workspace_leases (
                    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                    run_id TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    acquired_at INTEGER NOT NULL
                 );
                 INSERT INTO schema_migrations(version, applied_at)
                   VALUES (3, strftime('%s', 'now'));",
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn ensure_project(&self, root: &Path) -> Result<ProjectRecord> {
        let root_path = root.to_string_lossy().to_string();
        let name = root
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("workspace");
        let now = now();
        self.connection.execute(
            "INSERT INTO projects(root_path, display_name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(root_path) DO UPDATE SET updated_at = excluded.updated_at",
            params![root_path, name, now],
        )?;
        Ok(self.connection.query_row(
            "SELECT id, root_path, display_name FROM projects WHERE root_path = ?1",
            params![root_path],
            |row| {
                Ok(ProjectRecord {
                    id: row.get(0)?,
                    root_path: row.get(1)?,
                    display_name: row.get(2)?,
                })
            },
        )?)
    }

    pub fn list_conversations(&self, project_id: i64) -> Result<Vec<ConversationRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, title, updated_at FROM conversations WHERE project_id = ?1 ORDER BY updated_at DESC, id DESC",
        )?;
        let rows = statement.query_map(params![project_id], |row| {
            Ok(ConversationRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_conversation(&self, project_id: i64, title: &str) -> Result<ConversationRecord> {
        let now = now();
        self.connection.execute(
            "INSERT INTO conversations(project_id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![project_id, title, now],
        )?;
        Ok(ConversationRecord {
            id: self.connection.last_insert_rowid(),
            title: title.to_string(),
            updated_at: now,
        })
    }

    pub fn rename_conversation(&self, conversation_id: i64, title: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now(), conversation_id],
        )?;
        Ok(())
    }

    pub fn list_messages(&self, conversation_id: i64) -> Result<Vec<MessageRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, role, content, created_at FROM events WHERE conversation_id = ?1 AND kind = 'message' ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![conversation_id], |row| {
            Ok(MessageRecord {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn list_events(&self, conversation_id: i64) -> Result<Vec<EventRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT id, conversation_id, sequence, kind, role, content, event_key,
                    event_scope, provider_session_id, tool_call_id, message_id,
                    operation_status, metadata_json, created_at
             FROM events WHERE conversation_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map(params![conversation_id], event_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn build_handoff_context(&self, conversation_id: i64) -> Result<HandoffContext> {
        let events = self.list_events(conversation_id)?;
        Ok(HandoffContext {
            conversation_events: events
                .into_iter()
                .map(|event| ContextEvent {
                    event_id: event.id,
                    kind: event.kind,
                    event_key: event.event_key,
                    event_scope: event.event_scope,
                    role: event.role.unwrap_or_default(),
                    content: event.content.unwrap_or_default(),
                    sequence: event.sequence,
                    provider_session_id: event.provider_session_id,
                    tool_call_id: event.tool_call_id,
                    message_id: event.message_id,
                    operation_status: event.operation_status,
                    metadata_json: event.metadata_json,
                })
                .collect(),
            ..HandoffContext::default()
        })
    }

    pub fn start_provider_run(
        &self,
        project_id: i64,
        conversation_id: i64,
        run_id: &str,
        provider: &str,
        native_session_id: Option<&str>,
    ) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        let acquired = tx.execute(
            "INSERT INTO workspace_leases(project_id, run_id, provider, acquired_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![project_id, run_id, provider, now()],
        );
        if let Err(error) = acquired {
            anyhow::bail!("workspace is already owned by another provider run: {error}");
        }
        tx.execute(
            "INSERT INTO provider_sessions(conversation_id, provider, native_session_id,
                    status, started_at, run_id)
             VALUES (?1, ?2, ?3, 'running', ?4, ?5)",
            params![conversation_id, provider, native_session_id, now(), run_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn finish_provider_run(&self, project_id: i64, run_id: &str, status: &str) -> Result<()> {
        let tx = self.connection.unchecked_transaction()?;
        tx.execute(
            "UPDATE provider_sessions SET status = ?1, ended_at = ?2 WHERE run_id = ?3",
            params![status, now(), run_id],
        )?;
        tx.execute(
            "DELETE FROM workspace_leases WHERE project_id = ?1 AND run_id = ?2",
            params![project_id, run_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn append_message(
        &mut self,
        conversation_id: i64,
        role: &str,
        content: &str,
        conversation_title: Option<&str>,
    ) -> Result<EventRecord> {
        self.append_event(
            conversation_id,
            EventInput {
                kind: "message",
                role: Some(role),
                content: Some(content),
                event_key: None,
                event_scope: None,
                provider_session_id: None,
                tool_call_id: None,
                message_id: None,
                operation_status: None,
                metadata_json: None,
                conversation_title,
            },
        )
    }

    pub fn append_event(
        &mut self,
        conversation_id: i64,
        event: EventInput<'_>,
    ) -> Result<EventRecord> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(event_key) = event.event_key
            && let Some(existing) = tx
                .query_row(
                    "SELECT id, conversation_id, sequence, kind, role, content, event_key,
                            event_scope, provider_session_id, tool_call_id, message_id,
                            operation_status, metadata_json, created_at
                     FROM events WHERE conversation_id = ?1 AND event_scope = ?2 AND event_key = ?3",
                    params![conversation_id, event.event_scope.unwrap_or("application"), event_key],
                    event_from_row,
                )
                .optional()?
        {
            if existing.kind != event.kind
                || existing.role.as_deref() != event.role
                || existing.content.as_deref() != event.content
                || existing.provider_session_id.as_deref() != event.provider_session_id
                || existing.operation_status.as_deref() != event.operation_status
                || existing.metadata_json.as_deref() != event.metadata_json
            {
                anyhow::bail!("conflicting payload for event scope/key");
            }
            tx.commit()?;
            return Ok(existing);
        }

        let sequence: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        let created_at = now();
        tx.execute(
            "INSERT INTO events(conversation_id, sequence, kind, role, content, event_key,
                                event_scope, provider_session_id, tool_call_id, message_id,
                                operation_status, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                conversation_id,
                sequence,
                event.kind,
                event.role,
                event.content,
                event.event_key,
                event.event_scope.unwrap_or("application"),
                event.provider_session_id,
                event.tool_call_id,
                event.message_id,
                event.operation_status,
                event.metadata_json,
                created_at
            ],
        )?;
        tx.execute(
            "UPDATE conversations
             SET updated_at = ?1, title = COALESCE(?2, title)
             WHERE id = ?3",
            params![created_at, event.conversation_title, conversation_id],
        )?;
        let record = EventRecord {
            id: tx.last_insert_rowid(),
            conversation_id,
            sequence,
            kind: event.kind.to_string(),
            role: event.role.map(str::to_string),
            content: event.content.map(str::to_string),
            event_key: event.event_key.map(str::to_string),
            event_scope: event.event_scope.unwrap_or("application").to_string(),
            provider_session_id: event.provider_session_id.map(str::to_string),
            tool_call_id: event.tool_call_id.map(str::to_string),
            message_id: event.message_id.map(str::to_string),
            operation_status: event.operation_status.map(str::to_string),
            metadata_json: event.metadata_json.map(str::to_string),
            created_at,
        };
        tx.commit()?;
        Ok(record)
    }
}

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<EventRecord> {
    Ok(EventRecord {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        sequence: row.get(2)?,
        kind: row.get(3)?,
        role: row.get(4)?,
        content: row.get(5)?,
        event_key: row.get(6)?,
        event_scope: row.get(7)?,
        provider_session_id: row.get(8)?,
        tool_call_id: row.get(9)?,
        message_id: row.get(10)?,
        operation_status: row.get(11)?,
        metadata_json: row.get(12)?,
        created_at: row.get(13)?,
    })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_and_messages_survive_reopen() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        fs::create_dir_all(&root)?;

        let mut database = Database::open(&root)?;
        let project = database.ensure_project(&root)?;
        let conversation = database.create_conversation(project.id, "First conversation")?;
        database.append_message(conversation.id, "user", "hello", None)?;
        drop(database);

        let reopened = Database::open(&root)?;
        let project = reopened.ensure_project(&root)?;
        let conversations = reopened.list_conversations(project.id)?;
        assert_eq!(conversations.len(), 1);
        assert_eq!(
            reopened.list_messages(conversations[0].id)?[0].content,
            "hello"
        );
        Ok(())
    }

    #[test]
    fn event_ingestion_is_idempotent_and_sequences_are_contiguous() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        fs::create_dir_all(&root)?;
        let mut database = Database::open(&root)?;
        let project = database.ensure_project(&root)?;
        let conversation = database.create_conversation(project.id, "New conversation")?;

        let input = EventInput {
            kind: "message.delta",
            role: Some("assistant"),
            content: Some("partial"),
            event_key: Some("provider:event-1"),
            event_scope: Some("provider/run-1/session-1"),
            provider_session_id: Some("session-1"),
            tool_call_id: None,
            message_id: Some("message-1"),
            operation_status: Some("running"),
            metadata_json: None,
            conversation_title: Some("Recovered conversation"),
        };
        let first = database.append_event(conversation.id, input)?;
        let duplicate = database.append_event(
            conversation.id,
            EventInput {
                kind: "message.delta",
                role: Some("assistant"),
                content: Some("partial"),
                event_key: Some("provider:event-1"),
                event_scope: Some("provider/run-1/session-1"),
                provider_session_id: Some("session-1"),
                tool_call_id: None,
                message_id: Some("message-1"),
                operation_status: Some("running"),
                metadata_json: None,
                conversation_title: Some("Should not duplicate"),
            },
        )?;
        let second = database.append_event(
            conversation.id,
            EventInput {
                kind: "message.completed",
                role: Some("assistant"),
                content: Some("done"),
                event_key: Some("provider:event-2"),
                event_scope: Some("provider/run-1/session-1"),
                provider_session_id: Some("session-1"),
                tool_call_id: None,
                message_id: Some("message-1"),
                operation_status: Some("completed"),
                metadata_json: None,
                conversation_title: None,
            },
        )?;

        assert_eq!(first.id, duplicate.id);
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(
            database.list_conversations(project.id)?[0].title,
            "Recovered conversation"
        );
        Ok(())
    }

    #[test]
    fn failed_event_insert_does_not_leave_partial_state() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        fs::create_dir_all(&root)?;
        let mut database = Database::open(&root)?;

        let result = database.append_event(
            999,
            EventInput {
                kind: "message",
                role: Some("user"),
                content: Some("orphan"),
                event_key: Some("orphan-1"),
                event_scope: None,
                provider_session_id: None,
                tool_call_id: None,
                message_id: None,
                operation_status: None,
                metadata_json: None,
                conversation_title: None,
            },
        );
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn restart_replays_interrupted_and_partial_tool_events_for_handoff() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        fs::create_dir_all(&root)?;
        let mut database = Database::open(&root)?;
        let project = database.ensure_project(&root)?;
        let conversation = database.create_conversation(project.id, "Continue")?;
        database.append_event(
            conversation.id,
            EventInput {
                kind: "tool.started",
                role: Some("assistant"),
                content: Some("running tests"),
                event_key: Some("tool-start"),
                event_scope: Some("run-1"),
                provider_session_id: Some("native-1"),
                tool_call_id: Some("call-1"),
                message_id: None,
                operation_status: Some("running"),
                metadata_json: Some(r#"{"command":"cargo test"}"#),
                conversation_title: None,
            },
        )?;
        database.append_event(
            conversation.id,
            EventInput {
                kind: "tool.finished",
                role: Some("assistant"),
                content: Some("provider stopped before confirmation"),
                event_key: Some("tool-interrupted"),
                event_scope: Some("run-1"),
                provider_session_id: Some("native-1"),
                tool_call_id: Some("call-1"),
                message_id: None,
                operation_status: Some("interrupted"),
                metadata_json: None,
                conversation_title: None,
            },
        )?;
        drop(database);

        let reopened = Database::open(&root)?;
        let context = reopened.build_handoff_context(conversation.id)?;
        assert_eq!(context.conversation_events.len(), 2);
        assert_eq!(
            context.conversation_events[1].tool_call_id.as_deref(),
            Some("call-1")
        );
        assert_eq!(
            context.conversation_events[1].operation_status.as_deref(),
            Some("interrupted")
        );
        assert_eq!(
            context.conversation_events[0].metadata_json.as_deref(),
            Some(r#"{"command":"cargo test"}"#)
        );
        Ok(())
    }

    #[test]
    fn conflicting_replay_is_rejected_and_workspace_ownership_is_exclusive() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("project");
        fs::create_dir_all(&root)?;
        let mut first = Database::open(&root)?;
        let project = first.ensure_project(&root)?;
        let conversation = first.create_conversation(project.id, "Ownership")?;
        let event = EventInput {
            kind: "notice",
            role: Some("system"),
            content: Some("original"),
            event_key: Some("event-1"),
            event_scope: Some("run-1"),
            provider_session_id: None,
            tool_call_id: None,
            message_id: None,
            operation_status: None,
            metadata_json: None,
            conversation_title: None,
        };
        first.append_event(conversation.id, event)?;
        assert!(
            first
                .append_event(
                    conversation.id,
                    EventInput {
                        content: Some("changed"),
                        ..event
                    }
                )
                .is_err()
        );
        first.start_provider_run(
            project.id,
            conversation.id,
            "run-1",
            "claude",
            Some("native-1"),
        )?;
        let second = Database::open(&root)?;
        assert!(
            second
                .start_provider_run(project.id, conversation.id, "run-2", "codex", None)
                .is_err()
        );
        first.finish_provider_run(project.id, "run-1", "interrupted")?;
        second.start_provider_run(project.id, conversation.id, "run-2", "codex", None)?;
        Ok(())
    }
}
