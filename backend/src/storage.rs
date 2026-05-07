use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::AppState;

#[derive(Error, Debug)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("not found")]
    NotFound,
    #[error("forbidden")]
    Forbidden,
}

#[derive(Clone)]
pub struct Storage {
    inner: Arc<Mutex<Connection>>,
}

#[derive(Debug, Serialize, Clone)]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Clone)]
pub struct Message {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    /// Base64 image attachments. Empty for non-image messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Lifecycle of the message. `done` for fully-persisted messages,
    /// `pending` for assistant rows awaiting generation, `error` for
    /// failed generations (with the error text in `content`).
    pub status: String,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                sub TEXT PRIMARY KEY,
                username TEXT NOT NULL,
                last_seen INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY,
                user_sub TEXT NOT NULL REFERENCES users(sub) ON DELETE CASCADE,
                title TEXT NOT NULL,
                model TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS conv_user_updated
                ON conversations(user_sub, updated_at DESC);
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conv_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                attachments TEXT
            );
            CREATE INDEX IF NOT EXISTS msg_conv ON messages(conv_id, id);
            "#,
        )?;
        // Idempotent migration for pre-attachments databases. SQLite's
        // CREATE TABLE IF NOT EXISTS doesn't add new columns to an existing
        // table, so try the ALTER and ignore the "duplicate column" error.
        if let Err(e) = conn.execute("ALTER TABLE messages ADD COLUMN attachments TEXT", []) {
            if !e.to_string().to_lowercase().contains("duplicate column") {
                tracing::debug!("attachments column migration noop: {e}");
            }
        }
        if let Err(e) = conn.execute(
            "ALTER TABLE messages ADD COLUMN status TEXT NOT NULL DEFAULT 'done'",
            [],
        ) {
            if !e.to_string().to_lowercase().contains("duplicate column") {
                tracing::debug!("status column migration noop: {e}");
            }
        }
        Ok(Self { inner: Arc::new(Mutex::new(conn)) })
    }

    pub fn upsert_user(&self, sub: &str, username: &str) -> Result<(), StorageError> {
        let now = Utc::now().timestamp();
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO users(sub, username, last_seen) VALUES(?1, ?2, ?3)
             ON CONFLICT(sub) DO UPDATE SET username=excluded.username, last_seen=excluded.last_seen",
            params![sub, username, now],
        )?;
        Ok(())
    }

    pub fn list_conversations(&self, user_sub: &str) -> Result<Vec<Conversation>, StorageError> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, title, model, created_at, updated_at
             FROM conversations WHERE user_sub = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([user_sub], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                model: row.get(2)?,
                created_at: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn create_conversation(
        &self,
        user_sub: &str,
        title: &str,
        model: Option<&str>,
    ) -> Result<Conversation, StorageError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO conversations(id, user_sub, title, model, created_at, updated_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, user_sub, title, model, now],
        )?;
        Ok(Conversation {
            id,
            title: title.to_string(),
            model: model.map(str::to_string),
            created_at: now,
            updated_at: now,
        })
    }

    pub fn delete_conversation(&self, user_sub: &str, id: &str) -> Result<(), StorageError> {
        let conn = self.inner.lock().unwrap();
        let owner: Option<String> = conn
            .query_row(
                "SELECT user_sub FROM conversations WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?;
        match owner {
            None => Err(StorageError::NotFound),
            Some(s) if s != user_sub => Err(StorageError::Forbidden),
            _ => {
                conn.execute("DELETE FROM conversations WHERE id = ?1", [id])?;
                Ok(())
            }
        }
    }

    pub fn get_conversation(
        &self,
        user_sub: &str,
        id: &str,
    ) -> Result<Conversation, StorageError> {
        let conn = self.inner.lock().unwrap();
        let conv = conn
            .query_row(
                "SELECT id, user_sub, title, model, created_at, updated_at
                 FROM conversations WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?;
        match conv {
            None => Err(StorageError::NotFound),
            Some((_, sub, _, _, _, _)) if sub != user_sub => Err(StorageError::Forbidden),
            Some((id, _, title, model, created_at, updated_at)) => Ok(Conversation {
                id,
                title,
                model,
                created_at,
                updated_at,
            }),
        }
    }

    pub fn list_messages(
        &self,
        user_sub: &str,
        conv_id: &str,
    ) -> Result<Vec<Message>, StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, role, content, created_at, attachments, status FROM messages
             WHERE conv_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([conv_id], |row| {
            let attachments: Option<String> = row.get(4)?;
            let images = attachments
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                .unwrap_or_default();
            Ok(Message {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                images,
                status: row.get(5)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn append_message(
        &self,
        user_sub: &str,
        conv_id: &str,
        role: &str,
        content: &str,
        images: &[String],
    ) -> Result<i64, StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let now = Utc::now().timestamp();
        let attachments_json = if images.is_empty() {
            None
        } else {
            serde_json::to_string(images).ok()
        };
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(conv_id, role, content, created_at, attachments)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![conv_id, role, content, now, attachments_json],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, conv_id],
        )?;
        Ok(id)
    }

    /// Insert an empty assistant placeholder with `status='pending'`.
    /// Used by long-running generations (image gen) so the row is visible
    /// immediately when the user reloads or navigates back.
    pub fn append_pending_assistant(
        &self,
        user_sub: &str,
        conv_id: &str,
    ) -> Result<i64, StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let now = Utc::now().timestamp();
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(conv_id, role, content, created_at, attachments, status)
             VALUES(?1, 'assistant', '', ?2, NULL, 'pending')",
            params![conv_id, now],
        )?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, conv_id],
        )?;
        Ok(id)
    }

    pub fn complete_message(
        &self,
        message_id: i64,
        content: &str,
        images: &[String],
    ) -> Result<(), StorageError> {
        let attachments_json = if images.is_empty() {
            None
        } else {
            serde_json::to_string(images).ok()
        };
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE messages SET content = ?1, attachments = ?2, status = 'done'
             WHERE id = ?3",
            params![content, attachments_json, message_id],
        )?;
        Ok(())
    }

    pub fn fail_message(
        &self,
        message_id: i64,
        error_text: &str,
    ) -> Result<(), StorageError> {
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE messages SET content = ?1, status = 'error' WHERE id = ?2",
            params![error_text, message_id],
        )?;
        Ok(())
    }

    /// Delete the named message and every message after it in the same
    /// conversation. Used by the "delete from here" + "regenerate" UI
    /// affordances. Errors if the row doesn't belong to the user's chat.
    pub fn delete_message_and_after(
        &self,
        user_sub: &str,
        conv_id: &str,
        message_id: i64,
    ) -> Result<(), StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let conn = self.inner.lock().unwrap();
        let exists: Option<i64> = conn
            .query_row(
                "SELECT id FROM messages WHERE id = ?1 AND conv_id = ?2",
                params![message_id, conv_id],
                |r| r.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(StorageError::NotFound);
        }
        conn.execute(
            "DELETE FROM messages WHERE conv_id = ?1 AND id >= ?2",
            params![conv_id, message_id],
        )?;
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, conv_id],
        )?;
        Ok(())
    }

    /// Mark `pending` rows older than `older_than_secs` as `error`. Run once
    /// at startup so messages stuck across a backend crash don't keep the UI
    /// in a forever-loading state.
    pub fn fail_stale_pending(&self, older_than_secs: i64) -> Result<usize, StorageError> {
        let cutoff = Utc::now().timestamp() - older_than_secs;
        let conn = self.inner.lock().unwrap();
        let n = conn.execute(
            "UPDATE messages SET status = 'error',
                content = 'generation interrupted'
             WHERE status = 'pending' AND created_at < ?1",
            [cutoff],
        )?;
        Ok(n)
    }

    pub fn set_conversation_model(
        &self,
        user_sub: &str,
        conv_id: &str,
        model: &str,
    ) -> Result<(), StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET model = ?1 WHERE id = ?2",
            params![model, conv_id],
        )?;
        Ok(())
    }

    pub fn rename_if_default(&self, conv_id: &str, new_title: &str) -> Result<(), StorageError> {
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET title = ?1
             WHERE id = ?2 AND title = 'new chat'",
            params![new_title, conv_id],
        )?;
        Ok(())
    }

    pub fn set_conversation_title(
        &self,
        user_sub: &str,
        conv_id: &str,
        new_title: &str,
    ) -> Result<(), StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET title = ?1 WHERE id = ?2",
            params![new_title, conv_id],
        )?;
        Ok(())
    }

    pub fn purge_older_than(&self, ttl_days: u32) -> Result<usize, StorageError> {
        let cutoff = Utc::now().timestamp() - i64::from(ttl_days) * 86_400;
        let conn = self.inner.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM conversations WHERE updated_at < ?1",
            [cutoff],
        )?;
        Ok(n)
    }
}

pub fn start_ttl_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(3600);
        loop {
            tokio::time::sleep(interval).await;
            match state.storage.purge_older_than(state.settings.chat_ttl_days) {
                Ok(n) if n > 0 => {
                    tracing::info!("ttl sweep: removed {n} conversations");
                }
                Ok(_) => {}
                Err(e) => tracing::error!("ttl sweep failed: {e}"),
            }
        }
    });
}
