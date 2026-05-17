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
    #[error("invalid input: {0}")]
    Invalid(String),
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
pub struct Document {
    pub id: i64,
    pub name: String,
    pub mime: String,
    pub size_bytes: i64,
    pub chunk_count: i64,
    pub embedding_model: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct StoredChunk {
    pub document_id: i64,
    pub document_name: String,
    pub embedding_model: String,
    pub content: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SearchHit {
    pub message_id: i64,
    pub conv_id: String,
    pub conv_title: String,
    pub role: String,
    pub created_at: i64,
    /// FTS5-rendered snippet with `[…]` markers around matched terms.
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: i64,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    /// Number of attachments in `message_images`. Bytes are loaded on
    /// demand so the chat list endpoint stays cheap.
    pub image_count: usize,
    /// True when the row has an inpaint mask attached (kind='mask').
    /// Lets the UI render the base image with a translucent mask
    /// overlay without round-tripping a separate HEAD request.
    pub has_mask: bool,
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
            CREATE TABLE IF NOT EXISTS message_images (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
                idx INTEGER NOT NULL,
                mime TEXT NOT NULL DEFAULT 'image/png',
                bytes BLOB NOT NULL,
                UNIQUE(message_id, idx)
            );
            CREATE INDEX IF NOT EXISTS msg_images_message
                ON message_images(message_id);
            CREATE TABLE IF NOT EXISTS documents (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_sub TEXT NOT NULL REFERENCES users(sub) ON DELETE CASCADE,
                name TEXT NOT NULL,
                mime TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                chunk_count INTEGER NOT NULL DEFAULT 0,
                embedding_model TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS documents_user
                ON documents(user_sub, created_at DESC);
            CREATE TABLE IF NOT EXISTS document_chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                document_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                content TEXT NOT NULL,
                embedding BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS document_chunks_doc
                ON document_chunks(document_id);
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
        // Distinguish ordinary attachments from auxiliary blobs like an
        // inpaint mask. Default 'image' keeps every pre-existing row
        // categorised as a base attachment, so legacy chat history and
        // img2img references still surface through the unchanged
        // reader paths.
        if let Err(e) = conn.execute(
            "ALTER TABLE message_images ADD COLUMN kind TEXT NOT NULL DEFAULT 'image'",
            [],
        ) {
            if !e.to_string().to_lowercase().contains("duplicate column") {
                tracing::debug!("message_images kind migration noop: {e}");
            }
        }
        migrate_attachments_to_blobs(&conn)?;
        bootstrap_search_index(&conn)?;
        // Idempotent migration for pre-RAG-rework databases. The
        // column carries the model name used to embed the document's
        // chunks so retrieval can use a matching query embedding.
        if let Err(e) = conn.execute(
            "ALTER TABLE documents ADD COLUMN embedding_model TEXT NOT NULL DEFAULT ''",
            [],
        ) {
            if !e.to_string().to_lowercase().contains("duplicate column") {
                tracing::debug!("embedding_model column migration noop: {e}");
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
        // Count only renderable attachments here; mask blobs (kind='mask')
        // are auxiliary inputs to the inpaint pipeline and shouldn't surface
        // in the message DTO's image_count or drive thumbnail rendering.
        let mut stmt = conn.prepare(
            "SELECT m.id, m.role, m.content, m.created_at, m.status,
                    (SELECT COUNT(*) FROM message_images mi
                     WHERE mi.message_id = m.id AND mi.kind = 'image')
                        AS image_count,
                    EXISTS(SELECT 1 FROM message_images mi
                           WHERE mi.message_id = m.id AND mi.kind = 'mask')
                        AS has_mask
             FROM messages m
             WHERE m.conv_id = ?1
             ORDER BY m.id ASC",
        )?;
        let rows = stmt.query_map([conv_id], |row| {
            Ok(Message {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
                status: row.get(4)?,
                image_count: row.get::<_, i64>(5)? as usize,
                has_mask: row.get::<_, i64>(6)? != 0,
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
        mask: Option<&str>,
    ) -> Result<i64, StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let now = Utc::now().timestamp();
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO messages(conv_id, role, content, created_at)
             VALUES(?1, ?2, ?3, ?4)",
            params![conv_id, role, content, now],
        )?;
        let id = conn.last_insert_rowid();
        if !images.is_empty() {
            insert_message_images(&conn, id, images)?;
        }
        if let Some(m) = mask.filter(|s| !s.is_empty()) {
            // Stash the mask after the base images so the UNIQUE(message_id,
            // idx) constraint stays satisfied. The kind column is what
            // partitions readers; idx is only used for ordering within
            // base images today.
            insert_message_mask(&conn, id, images.len(), m)?;
        }
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
        let conn = self.inner.lock().unwrap();
        let updated = conn.execute(
            "UPDATE messages SET content = ?1, status = 'done' WHERE id = ?2",
            params![content, message_id],
        )?;
        if updated == 0 {
            // The pending row was deleted while the job was in flight —
            // typically because the user cancelled or navigated away.
            // Skip the image insert; the FK on `message_images` would
            // otherwise blow up.
            tracing::info!(
                "complete_message: row {message_id} gone before persistence; \
                 dropping generated payload"
            );
            return Ok(());
        }
        // Defensive: a re-completed message shouldn't keep stale images.
        conn.execute(
            "DELETE FROM message_images WHERE message_id = ?1",
            params![message_id],
        )?;
        if !images.is_empty() {
            insert_message_images(&conn, message_id, images)?;
        }
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

    /// Fetch the raw bytes + MIME of a single image attachment. Used by
    /// the per-image HTTP endpoint so the chat list response no longer
    /// needs to inline full image data.
    pub fn get_message_image_bytes(
        &self,
        user_sub: &str,
        conv_id: &str,
        msg_id: i64,
        idx: usize,
    ) -> Result<(Vec<u8>, String), StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let conn = self.inner.lock().unwrap();
        // Verify the message belongs to the conversation before reading
        // bytes — message_images joins on message_id only.
        let owned: Option<i64> = conn
            .query_row(
                "SELECT id FROM messages WHERE id = ?1 AND conv_id = ?2",
                params![msg_id, conv_id],
                |r| r.get(0),
            )
            .optional()?;
        if owned.is_none() {
            return Err(StorageError::NotFound);
        }
        let row: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT bytes, mime FROM message_images
                 WHERE message_id = ?1 AND idx = ?2 AND kind = 'image'",
                params![msg_id, idx as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.ok_or(StorageError::NotFound)
    }

    /// Base64-encoded variant for the chat history rebuild path that
    /// forwards user-side images to Ollama as part of `messages[].images`.
    /// Excludes mask blobs — those are inpaint inputs, not part of the
    /// vision history sent to chat models.
    pub fn get_message_images_b64(
        &self,
        msg_id: i64,
    ) -> Result<Vec<String>, StorageError> {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT bytes FROM message_images
             WHERE message_id = ?1 AND kind = 'image' ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(params![msg_id], |r| r.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let bytes = r?;
            out.push(B64.encode(bytes));
        }
        Ok(out)
    }

    /// Raw bytes + MIME of a message's inpaint mask, if any. Used by the
    /// per-message mask endpoint and by edit/regenerate paths that need
    /// to re-supply the mask on the next /api/chat call.
    pub fn get_message_mask_bytes(
        &self,
        user_sub: &str,
        conv_id: &str,
        msg_id: i64,
    ) -> Result<(Vec<u8>, String), StorageError> {
        self.get_conversation(user_sub, conv_id)?;
        let conn = self.inner.lock().unwrap();
        let owned: Option<i64> = conn
            .query_row(
                "SELECT id FROM messages WHERE id = ?1 AND conv_id = ?2",
                params![msg_id, conv_id],
                |r| r.get(0),
            )
            .optional()?;
        if owned.is_none() {
            return Err(StorageError::NotFound);
        }
        let row: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT bytes, mime FROM message_images
                 WHERE message_id = ?1 AND kind = 'mask'
                 ORDER BY idx ASC LIMIT 1",
                params![msg_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.ok_or(StorageError::NotFound)
    }

    /// Base64 mask for the resend paths (edit / regenerate). Returns
    /// None when the message has no mask attached.
    pub fn get_message_mask_b64(
        &self,
        msg_id: i64,
    ) -> Result<Option<String>, StorageError> {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        let conn = self.inner.lock().unwrap();
        let bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT bytes FROM message_images
                 WHERE message_id = ?1 AND kind = 'mask'
                 ORDER BY idx ASC LIMIT 1",
                params![msg_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(bytes.map(|b| B64.encode(b)))
    }

    /// Delete every message *after* the given anchor row, leaving the
    /// anchor in place. Used by the "regenerate from this user turn"
    /// affordance — the user message stays put and only the assistant
    /// reply (if any) and anything after gets trimmed.
    pub fn delete_messages_after(
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
            "DELETE FROM messages WHERE conv_id = ?1 AND id > ?2",
            params![conv_id, message_id],
        )?;
        let now = Utc::now().timestamp();
        conn.execute(
            "UPDATE conversations SET updated_at = ?1 WHERE id = ?2",
            params![now, conv_id],
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

    /// Full-text search across the user's messages. Returns hits sorted
    /// newest-first with a short snippet around the matched terms. The
    /// query is pre-tokenised here so the caller can pass plain user
    /// input; each whitespace-separated word becomes a prefix-matched
    /// FTS5 phrase ANDed against the rest.
    pub fn search(
        &self,
        user_sub: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StorageError> {
        let fts = build_fts_query(query);
        if fts.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.conv_id, c.title, m.role, m.created_at,
                    snippet(messages_fts, 0, '[', ']', '…', 12)
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             JOIN conversations c ON c.id = m.conv_id
             WHERE c.user_sub = ?1
               AND messages_fts MATCH ?2
               AND m.status = 'done'
             ORDER BY m.created_at DESC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![user_sub, fts, limit as i64],
            |row| {
                Ok(SearchHit {
                    message_id: row.get(0)?,
                    conv_id: row.get(1)?,
                    conv_title: row.get(2)?,
                    role: row.get(3)?,
                    created_at: row.get(4)?,
                    snippet: row.get(5)?,
                })
            },
        )?;
        let hits: Vec<SearchHit> = rows.collect::<Result<_, _>>()?;
        tracing::debug!(
            user = user_sub,
            fts = %fts,
            hit_count = hits.len(),
            "search executed",
        );
        Ok(hits)
    }

    pub fn create_document(
        &self,
        user_sub: &str,
        name: &str,
        mime: &str,
        size_bytes: i64,
        embedding_model: &str,
    ) -> Result<i64, StorageError> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.inner.lock().unwrap();
        conn.execute(
            "INSERT INTO documents(
                user_sub, name, mime, size_bytes, chunk_count,
                embedding_model, created_at
             ) VALUES(?1, ?2, ?3, ?4, 0, ?5, ?6)",
            params![user_sub, name, mime, size_bytes, embedding_model, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn insert_chunks(
        &self,
        document_id: i64,
        chunks: &[(String, Vec<f32>)],
    ) -> Result<(), StorageError> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "INSERT INTO document_chunks(document_id, position, content, embedding)
             VALUES(?1, ?2, ?3, ?4)",
        )?;
        for (idx, (content, embedding)) in chunks.iter().enumerate() {
            let bytes = crate::rag::embedding_to_bytes(embedding);
            stmt.execute(params![document_id, idx as i64, content, bytes])?;
        }
        conn.execute(
            "UPDATE documents SET chunk_count = ?1 WHERE id = ?2",
            params![chunks.len() as i64, document_id],
        )?;
        Ok(())
    }

    pub fn list_documents(&self, user_sub: &str) -> Result<Vec<Document>, StorageError> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, mime, size_bytes, chunk_count,
                    embedding_model, created_at
             FROM documents WHERE user_sub = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([user_sub], |row| {
            Ok(Document {
                id: row.get(0)?,
                name: row.get(1)?,
                mime: row.get(2)?,
                size_bytes: row.get(3)?,
                chunk_count: row.get(4)?,
                embedding_model: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn delete_document(
        &self,
        user_sub: &str,
        document_id: i64,
    ) -> Result<(), StorageError> {
        let conn = self.inner.lock().unwrap();
        let owner: Option<String> = conn
            .query_row(
                "SELECT user_sub FROM documents WHERE id = ?1",
                [document_id],
                |r| r.get(0),
            )
            .optional()?;
        match owner {
            None => Err(StorageError::NotFound),
            Some(s) if s != user_sub => Err(StorageError::Forbidden),
            _ => {
                conn.execute(
                    "DELETE FROM documents WHERE id = ?1",
                    [document_id],
                )?;
                Ok(())
            }
        }
    }

    /// Load every chunk owned by `user_sub` along with its source
    /// document's name. Retrieval ranks these in-process via cosine
    /// against the embedded query.
    pub fn load_user_chunks(
        &self,
        user_sub: &str,
    ) -> Result<Vec<StoredChunk>, StorageError> {
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT dc.document_id, d.name, d.embedding_model,
                    dc.content, dc.embedding
             FROM document_chunks dc
             JOIN documents d ON d.id = dc.document_id
             WHERE d.user_sub = ?1
               AND d.embedding_model != ''",
        )?;
        let rows = stmt.query_map([user_sub], |row| {
            let bytes: Vec<u8> = row.get(4)?;
            Ok(StoredChunk {
                document_id: row.get(0)?,
                document_name: row.get(1)?,
                embedding_model: row.get(2)?,
                content: row.get(3)?,
                embedding: crate::rag::embedding_from_bytes(&bytes),
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Remove a user and everything that depends on them. FK cascades
    /// from `users(sub)` → `conversations.user_sub` →
    /// `messages.conv_id` → `message_images.message_id` so the single
    /// DELETE pulls every blob and row attributable to the user. Used
    /// by the account self-delete endpoint.
    pub fn delete_user(&self, user_sub: &str) -> Result<(), StorageError> {
        let conn = self.inner.lock().unwrap();
        conn.execute("DELETE FROM users WHERE sub = ?1", [user_sub])?;
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

/// Decode a slice of raw base64 image strings (no `data:` prefix) and
/// insert them into `message_images` keyed by index. The stored MIME is
/// sniffed from the bytes themselves so payloads with a spoofed claim
/// or unsupported format are rejected at the boundary.
fn insert_message_images(
    conn: &Connection,
    message_id: i64,
    b64_images: &[String],
) -> Result<(), StorageError> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let mut stmt = conn.prepare(
        "INSERT INTO message_images(message_id, idx, mime, bytes, kind)
         VALUES(?1, ?2, ?3, ?4, 'image')",
    )?;
    for (idx, s) in b64_images.iter().enumerate() {
        let bytes = B64
            .decode(s.as_bytes())
            .map_err(|e| StorageError::Invalid(format!("base64 decode: {e}")))?;
        let mime = crate::image_kind::detect(&bytes).ok_or_else(|| {
            StorageError::Invalid(
                "unsupported image format — accepts png, jpeg, webp, gif".into(),
            )
        })?;
        stmt.execute(params![message_id, idx as i64, mime, bytes])?;
    }
    Ok(())
}

/// Persist an inpaint mask alongside a message's base attachments. The
/// caller picks `idx_offset` so the mask lands past the last base image,
/// keeping `UNIQUE(message_id, idx)` satisfied without renumbering.
fn insert_message_mask(
    conn: &Connection,
    message_id: i64,
    idx_offset: usize,
    b64_mask: &str,
) -> Result<(), StorageError> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let bytes = B64
        .decode(b64_mask.as_bytes())
        .map_err(|e| StorageError::Invalid(format!("base64 decode (mask): {e}")))?;
    let mime = crate::image_kind::detect(&bytes).ok_or_else(|| {
        StorageError::Invalid("unsupported mask format — accepts png".into())
    })?;
    conn.execute(
        "INSERT INTO message_images(message_id, idx, mime, bytes, kind)
         VALUES(?1, ?2, ?3, ?4, 'mask')",
        params![message_id, idx_offset as i64, mime, bytes],
    )?;
    Ok(())
}

/// Create the FTS5 mirror of `messages.content` and keep it in sync
/// via INSERT / UPDATE / DELETE triggers. Backfills on first run so
/// existing conversations are immediately searchable. FTS5 is bundled
/// with the rusqlite feature set; absence here would be a build-time
/// regression rather than a runtime fallback we need to plan for.
fn bootstrap_search_index(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            content,
            content='messages',
            content_rowid='id',
            tokenize='unicode61 remove_diacritics 2'
        );
        CREATE TRIGGER IF NOT EXISTS messages_fts_ai
        AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS messages_fts_ad
        AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content)
                VALUES('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS messages_fts_au
        AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, content)
                VALUES('delete', old.id, old.content);
            INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
        END;
        "#,
    )?;
    // FTS5 external-content tables expose a `rebuild` command that
    // (re)populates the index from the underlying `messages` table.
    // Compare the **index** row count (the hidden `_docsize` shadow
    // table) against the live message count — `SELECT COUNT(*) FROM
    // messages_fts` proxies through to the content table and would
    // always report the source count, which silently masks an empty
    // index on the first run.
    let fts_docs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages_fts_docsize",
        [],
        |r| r.get(0),
    )?;
    let msg_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE content != ''",
        [],
        |r| r.get(0),
    )?;
    if fts_docs < msg_rows {
        conn.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
            [],
        )?;
        tracing::info!(
            "search index: rebuilt ({fts_docs} → {msg_rows} messages)"
        );
    }
    Ok(())
}

/// Turn raw user input into a defensible FTS5 query. Each whitespace
/// token becomes a bare prefix-match (`word*`) and tokens are ANDed
/// together. FTS5's prefix operator only applies to plain terms — the
/// previous `"word"*` form (phrase + prefix) is invalid grammar and
/// silently returns no matches. Characters FTS5 would interpret as
/// query syntax are stripped so a stray punctuation mark can't blow
/// up the parser.
fn build_fts_query(raw: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for token in raw.split_whitespace() {
        let cleaned: String = token
            .chars()
            .filter(|c| {
                !matches!(
                    *c,
                    '"' | '\'' | '(' | ')' | '*' | ':' | '!' | '^' | '+'
                )
            })
            .collect();
        if cleaned.is_empty() {
            continue;
        }
        parts.push(format!("{cleaned}*"));
    }
    parts.join(" ")
}

/// One-shot migration that moves any base64 image attachments from the
/// legacy `messages.attachments` JSON column into the BLOB-backed
/// `message_images` table. Idempotent — skips messages that already
/// have rows in `message_images`. Clears the JSON only after the
/// corresponding BLOBs are inserted to avoid data loss on partial runs.
fn migrate_attachments_to_blobs(conn: &Connection) -> Result<(), StorageError> {
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages
         WHERE attachments IS NOT NULL AND attachments != ''
           AND id NOT IN (SELECT DISTINCT message_id FROM message_images)",
        [],
        |r| r.get(0),
    )?;
    if pending == 0 {
        // Belt-and-braces: clear stragglers whose images were already
        // copied across in a previous run but the column wasn't nulled.
        conn.execute(
            "UPDATE messages SET attachments = NULL
             WHERE attachments IS NOT NULL
               AND id IN (SELECT DISTINCT message_id FROM message_images)",
            [],
        )?;
        return Ok(());
    }

    tracing::info!(
        "migrating {pending} message(s) from attachments JSON into message_images BLOBs"
    );

    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, attachments FROM messages
             WHERE attachments IS NOT NULL AND attachments != ''
               AND id NOT IN (SELECT DISTINCT message_id FROM message_images)",
        )?;
        let iter = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        iter.collect::<Result<_, _>>()?
    };

    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    for (msg_id, json) in rows {
        let images: Vec<String> = match serde_json::from_str(&json) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("skipping malformed attachments for msg {msg_id}: {e}");
                continue;
            }
        };
        for (idx, b64s) in images.into_iter().enumerate() {
            let bytes = match B64.decode(b64s.as_bytes()) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("skipping bad base64 in msg {msg_id} idx {idx}: {e}");
                    continue;
                }
            };
            conn.execute(
                "INSERT OR IGNORE INTO message_images(message_id, idx, mime, bytes)
                 VALUES(?1, ?2, 'image/png', ?3)",
                params![msg_id, idx as i64, bytes],
            )?;
        }
    }
    conn.execute(
        "UPDATE messages SET attachments = NULL
         WHERE attachments IS NOT NULL
           AND id IN (SELECT DISTINCT message_id FROM message_images)",
        [],
    )?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    fn fresh() -> Storage {
        Storage::open(Path::new(":memory:")).expect("in-memory db")
    }

    fn seed_users(s: &Storage) {
        s.upsert_user("a", "alice").unwrap();
        s.upsert_user("b", "bob").unwrap();
    }

    #[test]
    fn list_conversations_is_per_user() {
        let s = fresh();
        seed_users(&s);
        s.create_conversation("a", "alice's chat", None).unwrap();
        s.create_conversation("b", "bob's chat", None).unwrap();
        let alice = s.list_conversations("a").unwrap();
        let bob = s.list_conversations("b").unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(bob.len(), 1);
        assert_eq!(alice[0].title, "alice's chat");
        assert_eq!(bob[0].title, "bob's chat");
    }

    #[test]
    fn get_conversation_forbids_cross_user_access() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let err = s.get_conversation("b", &c.id).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
    }

    #[test]
    fn delete_conversation_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let err = s.delete_conversation("b", &c.id).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
        // Bob's attempted delete must not have removed the row.
        s.get_conversation("a", &c.id).expect("alice's convo still readable");
    }

    #[test]
    fn list_messages_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        s.append_message("a", &c.id, "user", "hi", &[], None).unwrap();
        let err = s.list_messages("b", &c.id).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
    }

    #[test]
    fn append_message_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let err = s.append_message("b", &c.id, "user", "hi", &[], None).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
        // No row was inserted.
        let alice_msgs = s.list_messages("a", &c.id).unwrap();
        assert!(alice_msgs.is_empty());
    }

    #[test]
    fn delete_messages_after_keeps_anchor_drops_rest() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let m1 = s.append_message("a", &c.id, "user", "first", &[], None).unwrap();
        s.append_message("a", &c.id, "assistant", "reply", &[], None).unwrap();
        s.append_message("a", &c.id, "user", "second", &[], None).unwrap();
        s.delete_messages_after("a", &c.id, m1).unwrap();
        let remaining = s.list_messages("a", &c.id).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, m1);
    }

    #[test]
    fn delete_messages_after_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let m1 = s.append_message("a", &c.id, "user", "first", &[], None).unwrap();
        let err = s.delete_messages_after("b", &c.id, m1).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
    }

    #[test]
    fn delete_message_and_after_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let mid = s.append_message("a", &c.id, "user", "hi", &[], None).unwrap();
        let err = s
            .delete_message_and_after("b", &c.id, mid)
            .unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
        let alice_msgs = s.list_messages("a", &c.id).unwrap();
        assert_eq!(alice_msgs.len(), 1);
    }

    #[test]
    fn set_conversation_model_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let err = s
            .set_conversation_model("b", &c.id, "evil")
            .unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
    }

    #[test]
    fn set_conversation_title_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let err = s
            .set_conversation_title("b", &c.id, "pwned")
            .unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
        let again = s.get_conversation("a", &c.id).unwrap();
        assert_eq!(again.title, "t");
    }

    #[test]
    fn get_message_image_bytes_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let raw = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let b64 = B64.encode(&raw);
        let mid = s.append_message("a", &c.id, "user", "img", &[b64], None).unwrap();
        let err = s
            .get_message_image_bytes("b", &c.id, mid, 0)
            .unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
        // Bytes still readable by the owner — assertion guards against an
        // accidental delete during the forbidden path.
        let (bytes, mime) = s
            .get_message_image_bytes("a", &c.id, mid, 0)
            .unwrap();
        assert_eq!(bytes, raw);
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn mask_persists_alongside_base_without_inflating_image_count() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let base = B64.encode(b"\x89PNG\r\n\x1a\nbase");
        let mask = B64.encode(b"\x89PNG\r\n\x1a\nmask");
        let mid = s
            .append_message(
                "a",
                &c.id,
                "user",
                "inpaint please",
                std::slice::from_ref(&base),
                Some(&mask),
            )
            .unwrap();

        // image_count is the surface the UI reads; mask shouldn't be
        // counted or rendered as a thumbnail.
        let msgs = s.list_messages("a", &c.id).unwrap();
        let row = msgs.iter().find(|m| m.id == mid).unwrap();
        assert_eq!(row.image_count, 1);

        let (base_bytes, _) = s.get_message_image_bytes("a", &c.id, mid, 0).unwrap();
        assert_eq!(B64.encode(&base_bytes), base);

        let (mask_bytes, _) = s.get_message_mask_bytes("a", &c.id, mid).unwrap();
        assert_eq!(B64.encode(&mask_bytes), mask);

        let mask_b64 = s.get_message_mask_b64(mid).unwrap();
        assert_eq!(mask_b64.as_deref(), Some(mask.as_str()));

        // Mask bytes must never leak through the vision-history reader.
        let history = s.get_message_images_b64(mid).unwrap();
        assert_eq!(history, vec![base.clone()]);
    }

    #[test]
    fn mask_is_dropped_when_message_deleted_by_anchor() {
        // The "regenerate from this user turn" path keeps the user row
        // (with its mask) and only trims rows strictly after it. The
        // "delete from here" path drops the user row + everything
        // after, which must also cascade the mask. Both behaviours
        // ride on the FK from message_images.message_id → messages.id,
        // so they need a regression test before subtle schema tweaks
        // (e.g. a future BLOB-store split) can sneak in and silently
        // strand mask rows.
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let base = B64.encode(b"\x89PNG\r\n\x1a\nbase");
        let mask = B64.encode(b"\x89PNG\r\n\x1a\nmask");
        let user_id = s
            .append_message(
                "a",
                &c.id,
                "user",
                "inpaint",
                std::slice::from_ref(&base),
                Some(&mask),
            )
            .unwrap();
        let asst_id = s
            .append_message("a", &c.id, "assistant", "ok", &[], None)
            .unwrap();
        // Regenerate-from-user: trims rows strictly AFTER the user
        // anchor. User row stays, so the mask must stay readable.
        s.delete_messages_after("a", &c.id, user_id).unwrap();
        let still_there = s.get_message_mask_b64(user_id).unwrap();
        assert_eq!(still_there.as_deref(), Some(mask.as_str()));
        // Assistant row was after the anchor — gone.
        assert!(s.get_message_mask_b64(asst_id).unwrap().is_none());

        // Delete-from-here on the user row should cascade the mask.
        s.delete_message_and_after("a", &c.id, user_id).unwrap();
        assert!(s.get_message_mask_b64(user_id).unwrap().is_none());
    }

    #[test]
    fn get_message_mask_b64_is_none_without_mask() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let mid = s
            .append_message("a", &c.id, "user", "no mask here", &[], None)
            .unwrap();
        assert!(s.get_message_mask_b64(mid).unwrap().is_none());
    }

    #[test]
    fn open_is_idempotent_for_new_columns() {
        // Re-opening the same on-disk database must not blow up on the
        // ALTER TABLE migrations — each ADD COLUMN runs once, the
        // second open observes "duplicate column" and treats it as a noop.
        let path = std::env::temp_dir().join(format!(
            "chat-test-{}.db",
            Uuid::new_v4()
        ));
        let _first = Storage::open(&path).expect("first open");
        drop(_first);
        let second = Storage::open(&path).expect("second open");
        drop(second);
        // Clean up the temp file + WAL/SHM siblings so subsequent test
        // runs don't accumulate stale databases.
        for suffix in ["", "-shm", "-wal"] {
            let _ = std::fs::remove_file(format!(
                "{}{}",
                path.display(),
                suffix
            ));
        }
    }

    #[test]
    fn append_message_rejects_non_image_bytes() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let bogus = B64.encode(b"not an image at all");
        let err = s
            .append_message("a", &c.id, "user", "img", std::slice::from_ref(&bogus), None)
            .unwrap_err();
        assert!(matches!(err, StorageError::Invalid(_)));
    }

    #[test]
    fn delete_conversation_cascades_messages_and_images() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let raw = B64.encode(b"\x89PNG\r\n\x1a\nfake");
        let mid = s
            .append_message("a", &c.id, "user", "hi", std::slice::from_ref(&raw), None)
            .unwrap();
        s.delete_conversation("a", &c.id).unwrap();
        // Messages + images are gone via FK cascade.
        let err = s.get_message_image_bytes("a", &c.id, mid, 0).unwrap_err();
        assert!(matches!(err, StorageError::NotFound | StorageError::Forbidden));
    }

    #[test]
    fn delete_user_cascades_everything() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let raw = B64.encode(b"\x89PNG\r\n\x1a\nfake");
        s.append_message("a", &c.id, "user", "hi", std::slice::from_ref(&raw), None)
            .unwrap();
        s.delete_user("a").unwrap();
        let err = s.list_messages("a", &c.id).unwrap_err();
        // The conversation row is gone, so get_conversation hits NotFound
        // before checking ownership.
        assert!(matches!(err, StorageError::NotFound));
        // Other user untouched.
        s.create_conversation("b", "still here", None).unwrap();
        let bob_convs = s.list_conversations("b").unwrap();
        assert_eq!(bob_convs.len(), 1);
    }

    #[test]
    fn search_returns_only_owners_hits() {
        let s = fresh();
        seed_users(&s);
        let a_conv = s.create_conversation("a", "alice room", None).unwrap();
        let b_conv = s.create_conversation("b", "bob room", None).unwrap();
        s.append_message("a", &a_conv.id, "user", "make a kanidm guide", &[], None)
            .unwrap();
        s.append_message("b", &b_conv.id, "user", "kanidm setup notes", &[], None)
            .unwrap();
        let hits = s.search("a", "kanidm", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conv_id, a_conv.id);
        assert!(hits[0].snippet.contains("kanidm"));
    }

    #[test]
    fn search_prefix_matches_partial_words() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        s.append_message("a", &c.id, "user", "implementing actix-web handlers", &[], None)
            .unwrap();
        let hits = s.search("a", "implement", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn search_skips_pending_assistant_rows() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let pending = s
            .append_pending_assistant("a", &c.id)
            .unwrap();
        // Empty pending row content has nothing to match anyway, but
        // verify the status filter holds even when content is set.
        s.fail_message(pending, "kanidm").unwrap();
        let hits = s.search("a", "kanidm", 10).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_empty_query_returns_no_hits() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        s.append_message("a", &c.id, "user", "hello world", &[], None).unwrap();
        assert!(s.search("a", "", 10).unwrap().is_empty());
        assert!(s.search("a", "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_handles_quotes_in_input() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        s.append_message("a", &c.id, "user", "quoted reply", &[], None).unwrap();
        let hits = s.search("a", "\"quoted\"", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn unknown_conversation_returns_not_found_not_forbidden() {
        let s = fresh();
        seed_users(&s);
        let err = s.get_conversation("a", "no-such-id").unwrap_err();
        assert!(matches!(err, StorageError::NotFound));
    }
}
