use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

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

pub struct EventInput<'a> {
    pub kind: &'a str,
    pub role: Option<&'a str>,
    pub content: Option<&'a str>,
    pub provider_session_id: Option<&'a str>,
    pub operation_status: Option<&'a str>,
    pub metadata_json: Option<&'a str>,
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
        let applied: Option<i64> = self
            .connection
            .query_row(
                "SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if applied.unwrap_or(0) < 1 {
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

    pub fn append_message(
        &self,
        conversation_id: i64,
        role: &str,
        content: &str,
    ) -> Result<MessageRecord> {
        self.append_event(
            conversation_id,
            EventInput {
                kind: "message",
                role: Some(role),
                content: Some(content),
                provider_session_id: None,
                operation_status: None,
                metadata_json: None,
            },
        )
    }

    pub fn append_event(
        &self,
        conversation_id: i64,
        event: EventInput<'_>,
    ) -> Result<MessageRecord> {
        let sequence: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE conversation_id = ?1",
            params![conversation_id],
            |row| row.get(0),
        )?;
        let created_at = now();
        self.connection.execute(
            "INSERT INTO events(conversation_id, sequence, kind, role, content, provider_session_id, operation_status, metadata_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                conversation_id,
                sequence,
                event.kind,
                event.role,
                event.content,
                event.provider_session_id,
                event.operation_status,
                event.metadata_json,
                created_at
            ],
        )?;
        self.connection.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![created_at, conversation_id],
        )?;
        Ok(MessageRecord {
            id: self.connection.last_insert_rowid(),
            role: event.role.unwrap_or("system").to_string(),
            content: event.content.unwrap_or_default().to_string(),
            created_at,
        })
    }
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

        let database = Database::open(&root)?;
        let project = database.ensure_project(&root)?;
        let conversation = database.create_conversation(project.id, "First conversation")?;
        database.append_message(conversation.id, "user", "hello")?;
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
}
