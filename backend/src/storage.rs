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
        migrate_attachments_to_blobs(&conn)?;
        bootstrap_search_index(&conn)?;
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
            "SELECT m.id, m.role, m.content, m.created_at, m.status,
                    (SELECT COUNT(*) FROM message_images mi WHERE mi.message_id = m.id)
                        AS image_count
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
        conn.execute(
            "UPDATE messages SET content = ?1, status = 'done' WHERE id = ?2",
            params![content, message_id],
        )?;
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
                 WHERE message_id = ?1 AND idx = ?2",
                params![msg_id, idx as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.ok_or(StorageError::NotFound)
    }

    /// Base64-encoded variant for the chat history rebuild path that
    /// forwards user-side images to Ollama as part of `messages[].images`.
    pub fn get_message_images_b64(
        &self,
        msg_id: i64,
    ) -> Result<Vec<String>, StorageError> {
        use base64::engine::general_purpose::STANDARD as B64;
        use base64::Engine;
        let conn = self.inner.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT bytes FROM message_images
             WHERE message_id = ?1 ORDER BY idx ASC",
        )?;
        let rows = stmt.query_map(params![msg_id], |r| r.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let bytes = r?;
            out.push(B64.encode(bytes));
        }
        Ok(out)
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
        "INSERT INTO message_images(message_id, idx, mime, bytes)
         VALUES(?1, ?2, ?3, ?4)",
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
    // Run it whenever the index is shorter than the live message
    // count — covers the first-time migration on an existing DB and
    // catches any drift from a missed trigger.
    let fts_rows: i64 =
        conn.query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))?;
    let msg_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE content != ''",
        [],
        |r| r.get(0),
    )?;
    if fts_rows < msg_rows {
        conn.execute(
            "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
            [],
        )?;
        tracing::info!(
            "search index: rebuilt ({fts_rows} → {msg_rows} messages)"
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
        s.append_message("a", &c.id, "user", "hi", &[]).unwrap();
        let err = s.list_messages("b", &c.id).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
    }

    #[test]
    fn append_message_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let err = s.append_message("b", &c.id, "user", "hi", &[]).unwrap_err();
        assert!(matches!(err, StorageError::Forbidden));
        // No row was inserted.
        let alice_msgs = s.list_messages("a", &c.id).unwrap();
        assert!(alice_msgs.is_empty());
    }

    #[test]
    fn delete_message_and_after_forbids_cross_user() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let mid = s.append_message("a", &c.id, "user", "hi", &[]).unwrap();
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
        let mid = s.append_message("a", &c.id, "user", "img", &[b64]).unwrap();
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
    fn append_message_rejects_non_image_bytes() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        let bogus = B64.encode(b"not an image at all");
        let err = s
            .append_message("a", &c.id, "user", "img", std::slice::from_ref(&bogus))
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
            .append_message("a", &c.id, "user", "hi", std::slice::from_ref(&raw))
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
        s.append_message("a", &c.id, "user", "hi", std::slice::from_ref(&raw))
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
        s.append_message("a", &a_conv.id, "user", "make a kanidm guide", &[])
            .unwrap();
        s.append_message("b", &b_conv.id, "user", "kanidm setup notes", &[])
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
        s.append_message("a", &c.id, "user", "implementing actix-web handlers", &[])
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
        s.append_message("a", &c.id, "user", "hello world", &[]).unwrap();
        assert!(s.search("a", "", 10).unwrap().is_empty());
        assert!(s.search("a", "   ", 10).unwrap().is_empty());
    }

    #[test]
    fn search_handles_quotes_in_input() {
        let s = fresh();
        seed_users(&s);
        let c = s.create_conversation("a", "t", None).unwrap();
        s.append_message("a", &c.id, "user", "quoted reply", &[]).unwrap();
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
