use serde::Serialize;
use sqlx::{SqlitePool, sqlite::{SqliteConnectOptions, SqlitePoolOptions}};
use std::str::FromStr;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE INDEX IF NOT EXISTS idx_messages_user ON messages(user_id, id);
CREATE TABLE IF NOT EXISTS reminders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL, title TEXT NOT NULL, when_text TEXT NOT NULL,
  done INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS vitals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT NOT NULL, kind TEXT NOT NULL, value TEXT NOT NULL, unit TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS chunks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id TEXT, source TEXT NOT NULL, text TEXT NOT NULL, embedding BLOB NOT NULL);
CREATE INDEX IF NOT EXISTS idx_chunks_user ON chunks(user_id, source);
CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY, tz TEXT NOT NULL DEFAULT 'UTC', tz_changed_at INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS plans (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, price_inr INTEGER NOT NULL,
  daily_tokens INTEGER NOT NULL, max_documents INTEGER NOT NULL, sort INTEGER NOT NULL DEFAULT 0,
  voice_minutes INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS subscriptions (
  user_id TEXT PRIMARY KEY, plan_id TEXT NOT NULL, period_start INTEGER NOT NULL, period_end INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, tokens INTEGER NOT NULL,
  kind TEXT NOT NULL, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_usage_user ON usage(user_id, created_at);
CREATE TABLE IF NOT EXISTS voice_usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, seconds INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_voice_user ON voice_usage(user_id, created_at);
CREATE TABLE IF NOT EXISTS orders (
  id TEXT PRIMARY KEY, user_id TEXT NOT NULL, plan_id TEXT NOT NULL, amount_inr INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending', provider_ref TEXT, created_at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_orders_user ON orders(user_id);
";

pub async fn init(url: &str) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(url)?.create_if_missing(true);
    // An in-memory database is per-connection, so keep a single connection for it.
    let max = if url.contains(":memory:") { 1 } else { 5 };
    let pool = SqlitePoolOptions::new().max_connections(max).connect_with(opts).await?;
    migrate_legacy(&pool).await?;
    for stmt in SCHEMA.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(&pool).await?;
    }
    // Databases created before voice minutes existed: add the column.
    let has_voice: Option<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('plans') WHERE name = 'voice_minutes'")
        .fetch_optional(&pool).await?;
    if has_voice.is_none() {
        sqlx::query("ALTER TABLE plans ADD COLUMN voice_minutes INTEGER NOT NULL DEFAULT 0").execute(&pool).await?;
    }
    Ok(pool)
}

/// Early dev databases used local passwords and monthly plans. Those tables cannot be mapped onto Clerk accounts
/// or daily plans, so they are dropped (chat data stays but belongs to ids that no longer exist).
async fn migrate_legacy(pool: &SqlitePool) -> anyhow::Result<()> {
    let legacy: Option<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('users') WHERE name = 'password_hash'")
        .fetch_optional(pool).await?;
    let old_plans: Option<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('plans') WHERE name = 'monthly_tokens'")
        .fetch_optional(pool).await?;
    if legacy.is_some() || old_plans.is_some() {
        tracing::warn!("migrating a pre-Clerk database: dropping old users, refresh_tokens, plans, subscriptions and usage tables");
        for t in ["users", "refresh_tokens", "plans", "subscriptions", "usage"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {t}"))).execute(pool).await?;
        }
    }
    Ok(())
}

/// Creates the local profile row on a user's first request (Clerk owns the identity).
pub async fn ensure_user(pool: &SqlitePool, id: &str, tz: &str) -> anyhow::Result<()> {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM users WHERE id = ?").bind(id).fetch_optional(pool).await?;
    if exists.is_none() {
        sqlx::query("INSERT OR IGNORE INTO users (id, tz, created_at) VALUES (?, ?, ?)")
            .bind(id).bind(tz).bind(crate::config::now()).execute(pool).await?;
    }
    Ok(())
}

/// Erases every piece of personal data. Order records are kept but unlinked (financial record keeping).
pub async fn erase_user(pool: &SqlitePool, id: &str) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for table in ["messages", "reminders", "vitals", "chunks", "usage", "voice_usage", "subscriptions"] {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table} WHERE user_id = ?"))).bind(id).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE orders SET user_id = 'deleted-account' WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Serialize, sqlx::FromRow, Debug)]
pub struct Reminder {
    pub id: i64,
    pub title: String,
    pub when_text: String,
    pub done: bool,
    pub created_at: String,
}

#[derive(Serialize, sqlx::FromRow, Debug)]
pub struct Vital {
    pub id: i64,
    pub kind: String,
    pub value: String,
    pub unit: String,
    pub created_at: String,
}

#[derive(Serialize, sqlx::FromRow, Debug)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub created_at: String,
}

pub async fn save_message(pool: &SqlitePool, user: &str, role: &str, content: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO messages (user_id, role, content) VALUES (?, ?, ?)")
        .bind(user).bind(role).bind(content).execute(pool).await?;
    Ok(())
}

/// Most recent `limit` messages, oldest first.
pub async fn recent_messages(pool: &SqlitePool, user: &str, limit: i64) -> anyhow::Result<Vec<StoredMessage>> {
    let mut rows: Vec<StoredMessage> = sqlx::query_as(
        "SELECT role, content, created_at FROM messages WHERE user_id = ? ORDER BY id DESC LIMIT ?")
        .bind(user).bind(limit).fetch_all(pool).await?;
    rows.reverse();
    Ok(rows)
}

pub async fn clear_history(pool: &SqlitePool, user: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM messages WHERE user_id = ?").bind(user).execute(pool).await?;
    Ok(())
}

pub async fn add_reminder(pool: &SqlitePool, user: &str, title: &str, when_text: &str) -> anyhow::Result<i64> {
    let r = sqlx::query("INSERT INTO reminders (user_id, title, when_text) VALUES (?, ?, ?)")
        .bind(user).bind(title).bind(when_text).execute(pool).await?;
    Ok(r.last_insert_rowid())
}

pub async fn list_reminders(pool: &SqlitePool, user: &str) -> anyhow::Result<Vec<Reminder>> {
    Ok(sqlx::query_as(
        "SELECT id, title, when_text, done, created_at FROM reminders WHERE user_id = ? ORDER BY done, id DESC")
        .bind(user).fetch_all(pool).await?)
}

pub async fn complete_reminder(pool: &SqlitePool, user: &str, id: i64) -> anyhow::Result<bool> {
    let r = sqlx::query("UPDATE reminders SET done = 1 WHERE id = ? AND user_id = ?")
        .bind(id).bind(user).execute(pool).await?;
    Ok(r.rows_affected() > 0)
}

pub async fn add_vital(pool: &SqlitePool, user: &str, kind: &str, value: &str, unit: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO vitals (user_id, kind, value, unit) VALUES (?, ?, ?, ?)")
        .bind(user).bind(kind).bind(value).bind(unit).execute(pool).await?;
    Ok(())
}

pub async fn list_vitals(pool: &SqlitePool, user: &str, kind: Option<&str>, limit: i64) -> anyhow::Result<Vec<Vital>> {
    Ok(match kind {
        Some(k) => sqlx::query_as(
            "SELECT id, kind, value, unit, created_at FROM vitals WHERE user_id = ? AND kind = ? ORDER BY id DESC LIMIT ?")
            .bind(user).bind(k).bind(limit).fetch_all(pool).await?,
        None => sqlx::query_as(
            "SELECT id, kind, value, unit, created_at FROM vitals WHERE user_id = ? ORDER BY id DESC LIMIT ?")
            .bind(user).bind(limit).fetch_all(pool).await?,
    })
}

pub struct Chunk {
    pub source: String,
    pub text: String,
    pub embedding: Vec<f32>,
}

fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn from_bytes(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub async fn insert_chunk(pool: &SqlitePool, user: Option<&str>, source: &str, text: &str, emb: &[f32]) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO chunks (user_id, source, text, embedding) VALUES (?, ?, ?, ?)")
        .bind(user).bind(source).bind(text).bind(to_bytes(emb)).execute(pool).await?;
    Ok(())
}

pub async fn delete_chunks_for_source(pool: &SqlitePool, user: &str, source: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM chunks WHERE user_id = ? AND source = ?").bind(user).bind(source).execute(pool).await?;
    Ok(())
}

pub async fn count_user_documents(pool: &SqlitePool, user: &str) -> anyhow::Result<i64> {
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(DISTINCT source) FROM chunks WHERE user_id = ?")
        .bind(user).fetch_one(pool).await?;
    Ok(n)
}

pub async fn count_chunks(pool: &SqlitePool, user: Option<&str>, source: &str) -> anyhow::Result<i64> {
    let (n,): (i64,) = match user {
        Some(u) => sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE user_id = ? AND source = ?")
            .bind(u).bind(source).fetch_one(pool).await?,
        None => sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE user_id IS NULL AND source = ?")
            .bind(source).fetch_one(pool).await?,
    };
    Ok(n)
}

/// `user = None` searches the shared knowledge base; `Some(id)` searches only that user's documents.
pub async fn load_chunks(pool: &SqlitePool, user: Option<&str>) -> anyhow::Result<Vec<Chunk>> {
    let rows: Vec<(String, String, Vec<u8>)> = match user {
        Some(u) => sqlx::query_as("SELECT source, text, embedding FROM chunks WHERE user_id = ?")
            .bind(u).fetch_all(pool).await?,
        None => sqlx::query_as("SELECT source, text, embedding FROM chunks WHERE user_id IS NULL")
            .fetch_all(pool).await?,
    };
    Ok(rows.into_iter().map(|(source, text, e)| Chunk { source, text, embedding: from_bytes(&e) }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn crud_roundtrip() {
        let pool = init("sqlite::memory:").await.unwrap();
        save_message(&pool, "u1", "user", "hi").await.unwrap();
        save_message(&pool, "u1", "assistant", "hello").await.unwrap();
        save_message(&pool, "u2", "user", "other").await.unwrap();
        let h = recent_messages(&pool, "u1", 10).await.unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].content, "hi");

        let id = add_reminder(&pool, "u1", "BP tablet", "8 pm daily").await.unwrap();
        assert!(!complete_reminder(&pool, "u2", id).await.unwrap(), "other users can't complete it");
        assert!(complete_reminder(&pool, "u1", id).await.unwrap());
        assert!(list_reminders(&pool, "u1").await.unwrap()[0].done);

        add_vital(&pool, "u1", "blood_pressure", "120/80", "mmHg").await.unwrap();
        assert_eq!(list_vitals(&pool, "u1", Some("blood_pressure"), 5).await.unwrap().len(), 1);
        assert_eq!(list_vitals(&pool, "u2", None, 5).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn chunk_scopes_are_separate() {
        let pool = init("sqlite::memory:").await.unwrap();
        insert_chunk(&pool, None, "kb.md", "global", &[1.0, 0.0]).await.unwrap();
        insert_chunk(&pool, Some("u1"), "report.txt", "private", &[0.0, 1.0]).await.unwrap();
        assert_eq!(load_chunks(&pool, None).await.unwrap().len(), 1);
        let mine = load_chunks(&pool, Some("u1")).await.unwrap();
        assert_eq!(mine[0].embedding, vec![0.0, 1.0]);
        assert_eq!(load_chunks(&pool, Some("u2")).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn legacy_database_is_migrated_and_users_can_be_erased() {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE users (id TEXT PRIMARY KEY, email TEXT, password_hash TEXT)").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE plans (id TEXT PRIMARY KEY, monthly_tokens INTEGER)").execute(&pool).await.unwrap();
        migrate_legacy(&pool).await.unwrap();
        for stmt in SCHEMA.split(';').map(str::trim).filter(|s| !s.is_empty()) { sqlx::query(stmt).execute(&pool).await.unwrap(); }
        let cols: Vec<(String,)> = sqlx::query_as("SELECT name FROM pragma_table_info('users')").fetch_all(&pool).await.unwrap();
        assert!(cols.iter().any(|c| c.0 == "tz") && !cols.iter().any(|c| c.0 == "password_hash"));

        ensure_user(&pool, "user_1", "Asia/Kolkata").await.unwrap();
        ensure_user(&pool, "user_1", "UTC").await.unwrap(); // second call must not overwrite
        let (tz,): (String,) = sqlx::query_as("SELECT tz FROM users WHERE id = 'user_1'").fetch_one(&pool).await.unwrap();
        assert_eq!(tz, "Asia/Kolkata");
        save_message(&pool, "user_1", "user", "hi").await.unwrap();
        add_vital(&pool, "user_1", "bp", "120/80", "").await.unwrap();
        sqlx::query("INSERT INTO orders (id, user_id, plan_id, amount_inr, created_at) VALUES ('o1','user_1','plus',199,0)").execute(&pool).await.unwrap();
        erase_user(&pool, "user_1").await.unwrap();
        assert!(recent_messages(&pool, "user_1", 10).await.unwrap().is_empty());
        assert!(list_vitals(&pool, "user_1", None, 10).await.unwrap().is_empty());
        let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 0);
        let (owner,): (String,) = sqlx::query_as("SELECT user_id FROM orders WHERE id = 'o1'").fetch_one(&pool).await.unwrap();
        assert_eq!(owner, "deleted-account", "order kept for records but unlinked");
    }
}
