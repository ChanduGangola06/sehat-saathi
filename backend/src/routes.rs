use crate::{agent, billing, clerk::{self, AuthUser}, db, error::AppError, rag, state::AppState};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::{delete, get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};

type Res<T> = Result<T, AppError>;

/// Per-user request cap for anything that costs money, on top of the daily token quota.
fn rate_limit(s: &AppState, user: &AuthUser) -> Res<()> {
    if s.limiter.allow(&format!("ai:{}", user.id), s.cfg.ai_rate_per_min, std::time::Duration::from_secs(60)) { Ok(()) }
    else { Err(AppError::too_many("You're sending requests too fast. Please wait a moment.")) }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/me", get(clerk::me))
        .route("/api/timezone", put(clerk::set_timezone).layer(DefaultBodyLimit::max(1024)))
        .route("/api/account", delete(clerk::delete_account))
        .route("/api/clerk/webhook", post(clerk::webhook).layer(DefaultBodyLimit::max(64 * 1024)))
        .route("/api/billing/plans", get(billing::plans))
        .route("/api/billing/status", get(billing::status))
        .route("/api/billing/checkout", post(billing::checkout))
        .route("/api/billing/dev-activate", post(billing::dev_activate))
        .route("/api/billing/webhook", post(billing::webhook).layer(DefaultBodyLimit::max(64 * 1024)))
        .route("/api/chat", post(chat).layer(DefaultBodyLimit::max(64 * 1024)))
        .route("/api/history", get(history).delete(clear_history))
        .route("/api/live", get(crate::live::ws_handler))
        .route("/api/reminders", get(reminders))
        .route("/api/reminders/{id}/done", post(reminder_done))
        .route("/api/vitals", get(vitals))
        .route("/api/documents", post(upload_document).layer(DefaultBodyLimit::max(1024 * 1024)))
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .with_state(state)
}

async fn health(State(s): State<AppState>) -> Json<Value> {
    Json(json!({ "ok": true, "ai_configured": s.ai.has_key(), "providers": s.ai.provider_names(), "chat_model": s.ai.chat_model }))
}

#[derive(Deserialize)]
struct ChatReq {
    message: String,
    #[serde(default = "default_lang")]
    lang: String,
}
fn default_lang() -> String { "auto".into() }

async fn chat(State(s): State<AppState>, user: AuthUser, Json(req): Json<ChatReq>) -> Res<Json<agent::AgentReply>> {
    rate_limit(&s, &user)?;
    let msg = req.message.trim();
    if msg.is_empty() { return Err(AppError::bad_request("message is empty")); }
    if msg.chars().count() > 4000 { return Err(AppError::bad_request("message too long")); }
    // Quota is enforced inside the agent, after the emergency check, so emergencies are always answered.
    Ok(Json(agent::run(&s, &user.id, msg, &req.lang).await?))
}

async fn history(State(s): State<AppState>, user: AuthUser) -> Res<Json<Value>> {
    Ok(Json(json!({ "messages": db::recent_messages(&s.pool, &user.id, 100).await? })))
}

async fn clear_history(State(s): State<AppState>, user: AuthUser) -> Res<Json<Value>> {
    db::clear_history(&s.pool, &user.id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn reminders(State(s): State<AppState>, user: AuthUser) -> Res<Json<Value>> {
    Ok(Json(json!({ "reminders": db::list_reminders(&s.pool, &user.id).await? })))
}

async fn reminder_done(State(s): State<AppState>, user: AuthUser, Path(id): Path<i64>) -> Res<Json<Value>> {
    if db::complete_reminder(&s.pool, &user.id, id).await? { Ok(Json(json!({ "ok": true }))) } else { Err(AppError::not_found("reminder not found")) }
}

async fn vitals(State(s): State<AppState>, user: AuthUser) -> Res<Json<Value>> {
    Ok(Json(json!({ "vitals": db::list_vitals(&s.pool, &user.id, None, 30).await? })))
}

#[derive(Deserialize)]
struct DocReq { name: String, text: String }

async fn upload_document(State(s): State<AppState>, user: AuthUser, Json(req): Json<DocReq>) -> Res<Json<Value>> {
    rate_limit(&s, &user)?;
    let name: String = req.name.chars().filter(|c| !c.is_control()).take(100).collect();
    if name.trim().is_empty() || req.text.trim().is_empty() { return Err(AppError::bad_request("name and text are required")); }
    if req.text.chars().count() > 200_000 { return Err(AppError::bad_request("document too large")); }
    let q = billing::ensure_quota(&s.pool, &user.id).await?;
    let existing = db::count_chunks(&s.pool, Some(&user.id), &name).await? > 0;
    if !existing && db::count_user_documents(&s.pool, &user.id).await? >= q.plan.max_documents {
        return Err(AppError(StatusCode::FORBIDDEN, format!("Your {} plan allows {} documents. Delete one or upgrade.", q.plan.name, q.plan.max_documents)));
    }
    db::delete_chunks_for_source(&s.pool, &user.id, &name).await?; // re-uploading replaces the old version
    let (n, tokens) = rag::ingest_text(&s.pool, &s.ai, Some(&user.id), &name, &req.text).await?;
    billing::record_usage(&s.pool, &user.id, tokens as i64, "embed").await?;
    Ok(Json(json!({ "ok": true, "chunks": n })))
}
