//! Subscription plans, DAILY token metering and (provider-agnostic) payment confirmation.
//!
//! Every plan has a `daily_tokens` allowance. The day is the user's own local day: the allowance resets at
//! 00:00 in the user's stored IANA timezone (e.g. Asia/Kolkata), handling DST. Unused tokens do not roll over.
//! "Tokens" are real model tokens for typed chat and embeddings. Live voice is metered separately, in minutes per day.
//! Paid plans are activated ONLY by a signed webhook from your payment provider
//! (or by the dev-only endpoint when BILLING_DEV_MODE=true); a user can never upgrade themselves.

use crate::{clerk::AuthUser, config::now, error::{AppError, QuotaExceeded}, state::AppState};
use anyhow::Result;
use axum::{Json, body::Bytes, extract::State, http::HeaderMap};
use chrono::{Duration, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::SqlitePool;

/// A paid plan lasts 30 days from activation, then falls back to Free unless renewed.
const PERIOD_SECS: i64 = 30 * 24 * 3600;
pub const DEFAULT_FREE_DAILY: i64 = 1000;

#[derive(Serialize, sqlx::FromRow, Clone, Debug)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub price_inr: i64,
    pub daily_tokens: i64,
    pub max_documents: i64,
    /// Live voice conversation time per day.
    pub voice_minutes: i64,
}

/// Starter plans. Prices and limits are examples: edit the `plans` table (or this list) to suit your costs.
pub async fn seed_plans(pool: &SqlitePool) -> Result<()> {
    let free_env: Option<i64> = std::env::var("FREE_DAILY_TOKENS").ok().and_then(|v| v.parse().ok());
    let free_tokens = free_env.unwrap_or(DEFAULT_FREE_DAILY);
    for (id, name, price, tokens, docs, sort, voice) in [
        ("free", "Free", 0, free_tokens, 2, 0, 2),
        ("plus", "Plus", 199, 2_000, 20, 1, 15),
        ("pro", "Pro", 499, 4_000, 100, 2, 45),
    ] {
        sqlx::query("INSERT INTO plans (id, name, price_inr, daily_tokens, max_documents, sort) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, price_inr = excluded.price_inr, daily_tokens = excluded.daily_tokens,
               max_documents = excluded.max_documents, sort = excluded.sort WHERE excluded.id != 'free'")
            .bind(id).bind(name).bind(price).bind(tokens).bind(docs).bind(sort).execute(pool).await?;
        sqlx::query("UPDATE plans SET voice_minutes = ? WHERE id = ?").bind(voice).bind(id).execute(pool).await?;
    }
    if free_env.is_some() {
        sqlx::query("UPDATE plans SET daily_tokens = ? WHERE id = 'free'").bind(free_tokens).execute(pool).await?;
    }
    Ok(())
}

pub async fn list_plans(pool: &SqlitePool) -> Result<Vec<Plan>> {
    Ok(sqlx::query_as("SELECT id, name, price_inr, daily_tokens, max_documents, voice_minutes FROM plans ORDER BY sort").fetch_all(pool).await?)
}

async fn get_plan(pool: &SqlitePool, id: &str) -> Result<Option<Plan>> {
    Ok(sqlx::query_as("SELECT id, name, price_inr, daily_tokens, max_documents, voice_minutes FROM plans WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

// ---------- the user's local day ----------

/// First instant of `date` in `tz`. If local midnight does not exist (a DST gap) the day starts at the first
/// valid instant after it; if it happens twice, the earlier one is used.
fn local_midnight(tz: Tz, date: NaiveDate) -> i64 {
    let mut t = date.and_hms_opt(0, 0, 0).expect("midnight is valid");
    for _ in 0..48 {
        match tz.from_local_datetime(&t) {
            LocalResult::Single(d) => return d.timestamp(),
            LocalResult::Ambiguous(first, _) => return first.timestamp(),
            LocalResult::None => t += Duration::minutes(30),
        }
    }
    date.and_hms_opt(0, 0, 0).expect("midnight is valid").and_utc().timestamp()
}

/// `[start, end)` of the user's local day containing `now_ts`, as unix seconds. `end` is when tokens reset.
pub fn day_window(tz: Tz, now_ts: i64) -> (i64, i64) {
    let local = Utc.timestamp_opt(now_ts, 0).single().unwrap_or_else(Utc::now).with_timezone(&tz);
    let date = local.date_naive();
    let next = date.succ_opt().unwrap_or(date);
    (local_midnight(tz, date), local_midnight(tz, next))
}

pub fn parse_tz(name: &str) -> Option<Tz> {
    if name.len() > 64 { return None; }
    name.parse::<Tz>().ok()
}

async fn user_tz(pool: &SqlitePool, user: &str) -> Result<Tz> {
    let row: Option<(String,)> = sqlx::query_as("SELECT tz FROM users WHERE id = ?").bind(user).fetch_optional(pool).await?;
    Ok(row.and_then(|(n,)| parse_tz(&n)).unwrap_or(Tz::UTC))
}

// ---------- subscription + quota ----------

/// Returns the active plan id, creating the free plan or dropping an expired paid plan back to free.
pub async fn get_subscription(pool: &SqlitePool, user: &str) -> Result<String> {
    let t = now();
    let row: Option<(String, i64)> = sqlx::query_as("SELECT plan_id, period_end FROM subscriptions WHERE user_id = ?")
        .bind(user).fetch_optional(pool).await?;
    match row {
        None => {
            // period_end = 0 means "never expires" (the free plan).
            sqlx::query("INSERT OR IGNORE INTO subscriptions (user_id, plan_id, period_start, period_end) VALUES (?, 'free', ?, 0)")
                .bind(user).bind(t).execute(pool).await?;
            Ok("free".into())
        }
        Some((_, end)) if end != 0 && end <= t => {
            sqlx::query("UPDATE subscriptions SET plan_id = 'free', period_start = ?, period_end = 0 WHERE user_id = ?")
                .bind(t).bind(user).execute(pool).await?;
            Ok("free".into())
        }
        Some((plan, _)) => Ok(plan),
    }
}

#[derive(Serialize, Debug)]
pub struct Quota {
    pub plan: Plan,
    pub limit: i64,
    pub used: i64,
    pub remaining: i64,
    /// Live voice time today, in seconds.
    pub voice_limit_secs: i64,
    pub voice_used_secs: i64,
    pub voice_remaining_secs: i64,
    /// Unix seconds of the next reset (the user's next local midnight).
    pub resets_at: i64,
    pub timezone: String,
}

pub async fn quota_at(pool: &SqlitePool, user: &str, now_ts: i64) -> Result<Quota> {
    let plan_id = get_subscription(pool, user).await?;
    let plan = get_plan(pool, &plan_id).await?.ok_or_else(|| anyhow::anyhow!("plan {plan_id} missing"))?;
    let tz = user_tz(pool, user).await?;
    let (start, end) = day_window(tz, now_ts);
    let (used,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(tokens), 0) FROM usage WHERE user_id = ? AND created_at >= ? AND created_at < ?")
        .bind(user).bind(start).bind(end).fetch_one(pool).await?;
    let (voice_used,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(seconds), 0) FROM voice_usage WHERE user_id = ? AND created_at >= ? AND created_at < ?")
        .bind(user).bind(start).bind(end).fetch_one(pool).await?;
    let voice_limit = plan.voice_minutes * 60;
    Ok(Quota {
        limit: plan.daily_tokens, used, remaining: (plan.daily_tokens - used).max(0),
        voice_limit_secs: voice_limit, voice_used_secs: voice_used, voice_remaining_secs: (voice_limit - voice_used).max(0),
        resets_at: end, timezone: tz.name().to_string(), plan,
    })
}

pub async fn quota(pool: &SqlitePool, user: &str) -> Result<Quota> {
    quota_at(pool, user, now()).await
}

/// Fails with `QuotaExceeded` (HTTP 402) when nothing is left today. A single request may overshoot slightly,
/// because the real token count is only known after the model answers.
pub async fn ensure_quota(pool: &SqlitePool, user: &str) -> Result<Quota> {
    let q = quota(pool, user).await?;
    if q.remaining <= 0 {
        return Err(QuotaExceeded.into());
    }
    Ok(q)
}

/// Adds live-voice conversation time (seconds) to today's total.
pub async fn record_voice_seconds(pool: &SqlitePool, user: &str, seconds: i64) -> Result<()> {
    if seconds <= 0 { return Ok(()); }
    sqlx::query("INSERT INTO voice_usage (user_id, seconds, created_at) VALUES (?, ?, ?)")
        .bind(user).bind(seconds).bind(now()).execute(pool).await?;
    Ok(())
}

pub async fn record_usage(pool: &SqlitePool, user: &str, tokens: i64, kind: &str) -> Result<()> {
    if tokens <= 0 { return Ok(()); }
    sqlx::query("INSERT INTO usage (user_id, tokens, kind, created_at) VALUES (?, ?, ?, ?)")
        .bind(user).bind(tokens).bind(kind).bind(now()).execute(pool).await?;
    Ok(())
}

/// Starts a fresh 30-day period on `plan_id` (also used for renewals). Today's usage still counts.
pub async fn activate_plan(pool: &SqlitePool, user: &str, plan_id: &str) -> Result<()> {
    let t = now();
    sqlx::query("INSERT INTO subscriptions (user_id, plan_id, period_start, period_end) VALUES (?, ?, ?, ?)
                 ON CONFLICT(user_id) DO UPDATE SET plan_id = excluded.plan_id, period_start = excluded.period_start,
                 period_end = excluded.period_end")
        .bind(user).bind(plan_id).bind(t).bind(t + PERIOD_SECS).execute(pool).await?;
    Ok(())
}

// ---------- handlers ----------

pub async fn plans(State(s): State<AppState>) -> Result<Json<Value>, AppError> {
    Ok(Json(json!({ "plans": list_plans(&s.pool).await?, "dev_mode": s.cfg.dev_billing })))
}

pub async fn status(State(s): State<AppState>, user: AuthUser) -> Result<Json<Quota>, AppError> {
    Ok(Json(quota(&s.pool, &user.id).await?))
}

#[derive(Deserialize)]
pub struct CheckoutReq { plan_id: String }

/// Creates a pending order. Send the user to your payment provider with `order_id`; the provider's webhook completes it.
pub async fn checkout(State(s): State<AppState>, user: AuthUser, Json(req): Json<CheckoutReq>) -> Result<Json<Value>, AppError> {
    let plan = get_plan(&s.pool, &req.plan_id).await?.ok_or_else(|| AppError::bad_request("Unknown plan."))?;
    if plan.price_inr <= 0 { return Err(AppError::bad_request("The free plan needs no payment.")); }
    let order_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO orders (id, user_id, plan_id, amount_inr, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&order_id).bind(&user.id).bind(&plan.id).bind(plan.price_inr).bind(now()).execute(&s.pool).await?;
    tracing::info!("checkout: user {} created order {order_id} for plan {}", user.id, plan.id);
    Ok(Json(json!({ "order_id": order_id, "plan_id": plan.id, "amount_inr": plan.price_inr, "dev_mode": s.cfg.dev_billing })))
}

async fn complete_order(pool: &SqlitePool, order_id: &str, provider_ref: Option<&str>) -> Result<bool> {
    // Only pending -> paid transitions activate a plan, so repeated webhooks are harmless.
    let r = sqlx::query("UPDATE orders SET status = 'paid', provider_ref = ? WHERE id = ? AND status = 'pending'")
        .bind(provider_ref).bind(order_id).execute(pool).await?;
    if r.rows_affected() != 1 { return Ok(false); }
    let (user, plan): (String, String) = sqlx::query_as("SELECT user_id, plan_id FROM orders WHERE id = ?").bind(order_id).fetch_one(pool).await?;
    if user != "deleted-account" {
        activate_plan(pool, &user, &plan).await?;
        tracing::info!("order {order_id} paid: user {user} is now on plan {plan}");
    }
    Ok(true)
}

#[derive(Deserialize)]
pub struct DevActivate { order_id: String }

/// DEV ONLY: pretends the payment succeeded. Disabled unless BILLING_DEV_MODE=true (never allowed in production).
pub async fn dev_activate(State(s): State<AppState>, user: AuthUser, Json(req): Json<DevActivate>) -> Result<Json<Value>, AppError> {
    if !s.cfg.dev_billing { return Err(AppError::not_found("Not found.")); }
    let owned: Option<(String,)> = sqlx::query_as("SELECT id FROM orders WHERE id = ? AND user_id = ?").bind(&req.order_id).bind(&user.id).fetch_optional(&s.pool).await?;
    if owned.is_none() { return Err(AppError::not_found("Order not found.")); }
    let done = complete_order(&s.pool, &req.order_id, Some("dev")).await?;
    Ok(Json(json!({ "ok": true, "activated": done })))
}

pub fn verify_signature(secret: &str, body: &[u8], sig_hex: &str) -> bool {
    let Some(sig) = decode_hex(sig_hex.trim()) else { return false };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else { return false };
    mac.update(body);
    mac.verify_slice(&sig).is_ok() // constant-time comparison
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 { return None; }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[derive(Deserialize)]
struct WebhookBody { order_id: String, status: String, provider_ref: Option<String> }

/// Payment provider callback. Header `X-Signature` = hex(HMAC-SHA256(BILLING_WEBHOOK_SECRET, raw body)).
/// Adapt `WebhookBody` / signature scheme to your provider (Razorpay, Stripe, Cashfree...).
pub async fn webhook(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, AppError> {
    let Some(secret) = s.cfg.webhook_secret.as_deref() else {
        return Err(AppError(axum::http::StatusCode::SERVICE_UNAVAILABLE, "Billing is not configured.".into()));
    };
    let sig = headers.get("x-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !verify_signature(secret, &body, sig) {
        return Err(AppError::unauthorized("Invalid signature."));
    }
    let wb: WebhookBody = serde_json::from_slice(&body).map_err(|_| AppError::bad_request("Bad payload."))?;
    match wb.status.as_str() {
        "paid" => { complete_order(&s.pool, &wb.order_id, wb.provider_ref.as_deref()).await?; }
        "failed" => {
            sqlx::query("UPDATE orders SET status = 'failed' WHERE id = ? AND status = 'pending'").bind(&wb.order_id).execute(&s.pool).await?;
        }
        _ => return Err(AppError::bad_request("Unknown status.")),
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn setup() -> SqlitePool {
        let pool = db::init("sqlite::memory:").await.unwrap();
        seed_plans(&pool).await.unwrap();
        pool
    }

    fn ts(tz: Tz, y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
        tz.with_ymd_and_hms(y, m, d, hh, mm, 0).single().unwrap().timestamp()
    }

    #[test]
    fn window_follows_local_midnight_in_each_country() {
        let ist: Tz = "Asia/Kolkata".parse().unwrap();
        let now_ts = ts(ist, 2026, 11, 21, 17, 46);
        let (s, e) = day_window(ist, now_ts);
        assert_eq!(s, ts(ist, 2026, 11, 21, 0, 0));
        assert_eq!(e, ts(ist, 2026, 11, 22, 0, 0));
        assert_eq!(e - s, 86_400);
        // The same instant is a different local day in New York, so the reset moment differs.
        let ny: Tz = "America/New_York".parse().unwrap();
        let (_, e_ny) = day_window(ny, now_ts);
        assert_ne!(e, e_ny);
        assert_eq!(e_ny, ts(ny, 2026, 11, 22, 0, 0));
        // One second before and after midnight land in different windows.
        let (_, e1) = day_window(ist, e - 1);
        let (s2, _) = day_window(ist, e);
        assert_eq!(e1, e);
        assert_eq!(s2, e);
    }

    #[test]
    fn window_handles_dst_days() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let (s, e) = day_window(ny, ts(ny, 2026, 3, 8, 12, 0)); // spring forward: 23-hour day
        assert_eq!(e - s, 23 * 3600);
        let (s, e) = day_window(ny, ts(ny, 2026, 11, 1, 12, 0)); // fall back: 25-hour day
        assert_eq!(e - s, 25 * 3600);
        // Havana's clocks jump 00:00 -> 01:00, so local midnight does not exist that day.
        let hav: Tz = "America/Havana".parse().unwrap();
        let now_ts = ts(hav, 2026, 3, 8, 12, 0);
        let (s, e) = day_window(hav, now_ts);
        assert!(s <= now_ts && now_ts < e, "window must contain now");
        assert!(e - s >= 22 * 3600 && e - s <= 24 * 3600);
    }

    #[test]
    fn timezone_names_are_validated() {
        assert!(parse_tz("Asia/Kolkata").is_some());
        assert!(parse_tz("Not/AZone").is_none());
        assert!(parse_tz(&"a".repeat(100)).is_none());
        assert!(parse_tz("").is_none());
    }

    #[tokio::test]
    async fn free_plan_is_1000_a_day_and_exhaustion_blocks() {
        let pool = setup().await;
        let q = quota(&pool, "u1").await.unwrap();
        assert_eq!(q.plan.id, "free");
        assert_eq!(q.limit, 1000);
        assert_eq!(q.remaining, 1000);
        record_usage(&pool, "u1", 400, "chat").await.unwrap();
        assert_eq!(quota(&pool, "u1").await.unwrap().remaining, 600);
        record_usage(&pool, "u1", 700, "chat").await.unwrap(); // overshoot by one turn is allowed
        let q = quota(&pool, "u1").await.unwrap();
        assert_eq!(q.remaining, 0);
        let err = ensure_quota(&pool, "u1").await.unwrap_err();
        assert!(err.downcast_ref::<QuotaExceeded>().is_some());
        assert!(ensure_quota(&pool, "u2").await.is_ok(), "other users unaffected");
    }

    #[tokio::test]
    async fn allowance_returns_after_the_users_local_midnight() {
        let pool = setup().await;
        db::ensure_user(&pool, "u1", "Asia/Kolkata").await.unwrap();
        let ist: Tz = "Asia/Kolkata".parse().unwrap();
        let before = ts(ist, 2026, 11, 21, 23, 59);
        sqlx::query("INSERT INTO usage (user_id, tokens, kind, created_at) VALUES ('u1', 1000, 'chat', ?)").bind(before).execute(&pool).await.unwrap();
        let q = quota_at(&pool, "u1", before + 30).await.unwrap();
        assert_eq!((q.used, q.remaining), (1000, 0), "still the same local day");
        assert_eq!(q.timezone, "Asia/Kolkata");
        assert_eq!(q.resets_at, ts(ist, 2026, 11, 22, 0, 0));
        let after = quota_at(&pool, "u1", before + 61).await.unwrap();
        assert_eq!((after.used, after.remaining), (0, 1000), "new local day, fresh allowance");
    }

    #[tokio::test]
    async fn same_instant_resets_at_different_times_in_different_countries() {
        let pool = setup().await;
        db::ensure_user(&pool, "ist", "Asia/Kolkata").await.unwrap();
        db::ensure_user(&pool, "nyc", "America/New_York").await.unwrap();
        let ist: Tz = "Asia/Kolkata".parse().unwrap();
        let t0 = ts(ist, 2026, 11, 21, 23, 0); // 11 pm in India, 12:30 pm in New York
        for u in ["ist", "nyc"] {
            sqlx::query("INSERT INTO usage (user_id, tokens, kind, created_at) VALUES (?, 1000, 'chat', ?)").bind(u).bind(t0).execute(&pool).await.unwrap();
        }
        let later = t0 + 2 * 3600; // 1 am in India (next day), 2:30 pm in New York
        assert_eq!(quota_at(&pool, "ist", later).await.unwrap().remaining, 1000);
        assert_eq!(quota_at(&pool, "nyc", later).await.unwrap().remaining, 0);
    }

    #[tokio::test]
    async fn voice_minutes_per_plan_and_daily_usage() {
        let pool = setup().await;
        let q = quota(&pool, "u1").await.unwrap();
        assert_eq!((q.voice_limit_secs, q.voice_remaining_secs), (120, 120), "free = 2 minutes");
        record_voice_seconds(&pool, "u1", 100).await.unwrap();
        assert_eq!(quota(&pool, "u1").await.unwrap().voice_remaining_secs, 20);
        record_voice_seconds(&pool, "u1", 500).await.unwrap();
        assert_eq!(quota(&pool, "u1").await.unwrap().voice_remaining_secs, 0, "never negative");
        activate_plan(&pool, "u1", "plus").await.unwrap();
        assert_eq!(quota(&pool, "u1").await.unwrap().voice_limit_secs, 15 * 60);
        activate_plan(&pool, "u1", "pro").await.unwrap();
        assert_eq!(quota(&pool, "u1").await.unwrap().voice_limit_secs, 45 * 60);
        // Voice time does not touch the token allowance.
        assert_eq!(quota(&pool, "u1").await.unwrap().used, 0);
    }

    #[tokio::test]
    async fn paid_plan_has_a_daily_limit_and_todays_usage_carries_over() {
        let pool = setup().await;
        let q = quota(&pool, "u1").await.unwrap();
        record_usage(&pool, "u1", q.limit, "chat").await.unwrap();
        assert!(ensure_quota(&pool, "u1").await.is_err());
        activate_plan(&pool, "u1", "plus").await.unwrap();
        let q = ensure_quota(&pool, "u1").await.unwrap();
        assert_eq!(q.plan.id, "plus");
        assert_eq!(q.limit, 2_000);
        assert_eq!(q.used, 1000, "today's earlier usage still counts");
        assert_eq!(q.remaining, 1_000);
    }

    #[tokio::test]
    async fn expired_paid_plan_falls_back_to_free() {
        let pool = setup().await;
        activate_plan(&pool, "u1", "pro").await.unwrap();
        sqlx::query("UPDATE subscriptions SET period_end = ? WHERE user_id = 'u1'").bind(now() - 1).execute(&pool).await.unwrap();
        assert_eq!(quota(&pool, "u1").await.unwrap().plan.id, "free");
        assert_eq!(quota(&pool, "u1").await.unwrap().limit, 1000);
    }

    #[tokio::test]
    async fn order_completion_is_idempotent() {
        let pool = setup().await;
        sqlx::query("INSERT INTO orders (id, user_id, plan_id, amount_inr, created_at) VALUES ('o1','u1','plus',199,0)").execute(&pool).await.unwrap();
        assert!(complete_order(&pool, "o1", Some("ref")).await.unwrap());
        assert!(!complete_order(&pool, "o1", Some("ref")).await.unwrap(), "second call does nothing");
        assert_eq!(quota(&pool, "u1").await.unwrap().plan.id, "plus");
    }

    #[test]
    fn webhook_signature() {
        let body = br#"{"order_id":"o1","status":"paid"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"whsec_test_secret_123456").unwrap();
        mac.update(body);
        let good: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(verify_signature("whsec_test_secret_123456", body, &good));
        assert!(!verify_signature("another-secret-value-xyz", body, &good));
        assert!(!verify_signature("whsec_test_secret_123456", b"tampered", &good));
        assert!(!verify_signature("whsec_test_secret_123456", body, "zz"));
        assert!(!verify_signature("whsec_test_secret_123456", body, ""));
    }
}
