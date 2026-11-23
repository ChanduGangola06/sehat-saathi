//! Authentication through Clerk (https://clerk.com).
//!
//! Clerk owns sign-up, sign-in, email verification, password reset and two-factor login. This server never sees a
//! password. The browser sends Clerk's short-lived session token as `Authorization: Bearer <jwt>`, and we:
//!   1. check it is RS256 and verify the signature with Clerk's public keys (JWKS, cached, refreshed on unknown `kid`),
//!   2. check `iss` (your Clerk Frontend API URL), `exp`, `nbf` and `azp` (the origin that requested the token),
//!   3. use `sub` (the Clerk user id) as the account id, creating our small local profile row on first use.
//!
//! Clerk also calls `POST /api/clerk/webhook` (signed with Svix) when a user is deleted, so personal data is erased
//! even when the account is removed from Clerk's own UI.

use crate::{billing, config::now, db, error::AppError, state::AppState};
use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Json,
    body::Bytes,
    extract::{FromRequestParts, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use std::{collections::HashMap, sync::RwLock, time::{Duration, Instant}};

const KEY_TTL: Duration = Duration::from_secs(3600);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(30);
const WEBHOOK_TOLERANCE_SECS: i64 = 300;
const TZ_COOLDOWN_SECS: i64 = 30 * 24 * 3600;

#[derive(Default)]
struct KeyCache {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Option<Instant>,
    last_attempt: Option<Instant>,
}

pub struct Clerk {
    issuer: String,
    jwks_url: String,
    api_url: String,
    secret_key: Option<String>,
    webhook_secret: Option<Vec<u8>>,
    authorized_parties: Vec<String>,
    http: reqwest::Client,
    cache: RwLock<KeyCache>,
    /// Only one key refresh runs at a time; everyone else waits for it and then reads the fresh cache.
    refresh_lock: tokio::sync::Mutex<()>,
}

#[derive(Deserialize)]
struct Jwk { kid: Option<String>, kty: String, n: Option<String>, e: Option<String> }
#[derive(Deserialize)]
struct JwkSet { keys: Vec<Jwk> }

#[derive(Deserialize)]
struct SessionClaims { sub: String, azp: Option<String> }

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

impl Clerk {
    pub fn from_env(production: bool, frontend_origin: &str) -> Result<Self> {
        let issuer = match env("CLERK_ISSUER") {
            Some(i) => i.trim_end_matches('/').to_string(),
            None => bail!("CLERK_ISSUER is required (your Clerk Frontend API URL, e.g. https://example-12.clerk.accounts.dev)"),
        };
        let jwks_url = env("CLERK_JWKS_URL").unwrap_or_else(|| format!("{issuer}/.well-known/jwks.json"));
        let secret_key = env("CLERK_SECRET_KEY");
        let webhook_secret = env("CLERK_WEBHOOK_SECRET").map(|s| parse_whsec(&s)).transpose()?;
        if production && secret_key.is_none() {
            bail!("CLERK_SECRET_KEY is required when APP_ENV=production (needed to delete accounts)");
        }
        if secret_key.is_none() {
            tracing::warn!("CLERK_SECRET_KEY not set: deleting an account will erase local data but not the Clerk user");
        }
        if webhook_secret.is_none() {
            tracing::warn!("CLERK_WEBHOOK_SECRET not set: /api/clerk/webhook is disabled");
        }
        let authorized_parties = env("CLERK_AUTHORIZED_PARTIES")
            .map(|v| v.split(',').map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_else(|| vec![frontend_origin.trim_end_matches('/').to_string()]);
        Ok(Self {
            issuer, jwks_url,
            api_url: env("CLERK_API_URL").unwrap_or_else(|| "https://api.clerk.com".into()).trim_end_matches('/').to_string(),
            secret_key, webhook_secret, authorized_parties,
            http: reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?,
            cache: RwLock::new(KeyCache::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
        })
    }

    #[cfg(test)]
    pub fn for_test(issuer: &str, parties: &[&str], webhook_secret: Option<&str>) -> Self {
        Self {
            issuer: issuer.into(), jwks_url: String::new(), api_url: String::new(), secret_key: None,
            webhook_secret: webhook_secret.map(|s| parse_whsec(s).unwrap()),
            authorized_parties: parties.iter().map(|s| s.to_string()).collect(),
            http: reqwest::Client::new(), cache: RwLock::new(KeyCache::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub fn insert_key(&self, kid: &str, key: DecodingKey) {
        let mut c = self.cache.write().unwrap();
        c.keys.insert(kid.into(), key);
        c.fetched_at = Some(Instant::now());
    }

    fn cached(&self, kid: &str) -> Option<DecodingKey> {
        self.cache.read().unwrap_or_else(|e| e.into_inner()).keys.get(kid).cloned()
    }

    fn stale(&self) -> bool {
        self.cache.read().unwrap_or_else(|e| e.into_inner()).fetched_at.is_none_or(|t| t.elapsed() > KEY_TTL)
    }

    async fn refresh_keys(&self) -> Result<()> {
        {
            let mut c = self.cache.write().unwrap_or_else(|e| e.into_inner());
            if c.last_attempt.is_some_and(|t| t.elapsed() < REFRESH_COOLDOWN) { return Ok(()); }
            c.last_attempt = Some(Instant::now()); // unknown-kid floods cannot make us hammer Clerk
        }
        let set: JwkSet = self.http.get(&self.jwks_url).send().await?.error_for_status()?.json().await.context("bad JWKS body")?;
        let mut keys = HashMap::new();
        for k in set.keys {
            if let (true, Some(kid), Some(n), Some(e)) = (k.kty == "RSA", k.kid, k.n, k.e) {
                keys.insert(kid, DecodingKey::from_rsa_components(&n, &e)?);
            }
        }
        if keys.is_empty() { bail!("JWKS contained no RSA keys"); }
        let mut c = self.cache.write().unwrap_or_else(|e| e.into_inner());
        c.keys = keys;
        c.fetched_at = Some(Instant::now());
        Ok(())
    }

    async fn key_for(&self, kid: &str) -> Option<DecodingKey> {
        if !self.stale() {
            if let Some(k) = self.cached(kid) { return Some(k); }
        }
        let _guard = self.refresh_lock.lock().await;
        // Another request may have refreshed while we waited for the lock (many requests arrive at once on a cold start).
        if !self.stale() {
            if let Some(k) = self.cached(kid) { return Some(k); }
        }
        if let Err(e) = self.refresh_keys().await {
            tracing::warn!("could not refresh Clerk keys: {e:#}");
        }
        self.cached(kid)
    }

    /// Verifies a Clerk session token and returns the Clerk user id (`sub`).
    pub async fn verify(&self, token: &str) -> Result<String> {
        let header = decode_header(token)?;
        if header.alg != Algorithm::RS256 { bail!("unexpected algorithm"); }
        let kid = header.kid.ok_or_else(|| anyhow!("token has no kid"))?;
        let key = self.key_for(&kid).await.ok_or_else(|| anyhow!("unknown signing key"))?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_issuer(&[self.issuer.as_str()]);
        v.set_required_spec_claims(&["exp", "sub", "iss"]);
        v.validate_nbf = true;
        v.validate_aud = false; // Clerk session tokens carry no audience
        v.leeway = 5;
        let claims = decode::<SessionClaims>(token, &key, &v)?.claims;
        // `azp` is the origin that asked Clerk for the token. Reject tokens minted for some other site.
        if let Some(azp) = claims.azp {
            if !self.authorized_parties.is_empty() && !self.authorized_parties.iter().any(|p| *p == azp.trim_end_matches('/')) {
                bail!("token was issued to an unauthorized party");
            }
        }
        Ok(claims.sub)
    }

    /// Deletes the user in Clerk (Backend API). Already-deleted users count as success.
    pub async fn delete_user(&self, user_id: &str) -> Result<()> {
        let Some(secret) = &self.secret_key else { return Ok(()) };
        let r = self.http.delete(format!("{}/v1/users/{}", self.api_url, urlencode(user_id)))
            .bearer_auth(secret).send().await?;
        if r.status().is_success() || r.status() == StatusCode::NOT_FOUND { Ok(()) } else { bail!("Clerk answered {}", r.status()) }
    }
}

fn urlencode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn parse_whsec(s: &str) -> Result<Vec<u8>> {
    let raw = s.strip_prefix("whsec_").unwrap_or(s);
    STANDARD.decode(raw).map_err(|_| anyhow!("CLERK_WEBHOOK_SECRET must look like whsec_<base64> (copy it from the Clerk dashboard)"))
}

/// Svix signature check: HMAC-SHA256 over `id.timestamp.body`, base64, header may hold several `v1,<sig>` values.
pub fn verify_svix(secret: &[u8], id: &str, timestamp: &str, body: &[u8], sig_header: &str, now_ts: i64) -> bool {
    let Ok(ts) = timestamp.parse::<i64>() else { return false };
    if (now_ts - ts).abs() > WEBHOOK_TOLERANCE_SECS { return false; } // replay protection
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else { return false };
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    sig_header.split(' ')
        .filter_map(|p| p.strip_prefix("v1,"))
        .filter_map(|s| STANDARD.decode(s).ok())
        .any(|sig| mac.clone().verify_slice(&sig).is_ok()) // constant-time
}

// ---------- extractor ----------

/// The signed-in user. Extracting it verifies the Clerk token and makes sure a local profile row exists.
pub struct AuthUser {
    pub id: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let token = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ")).ok_or_else(|| AppError::unauthorized("Please sign in."))?;
        let id = state.clerk.verify(token).await.map_err(|e| {
            tracing::warn!("auth rejected for {}: {e:#}", parts.uri.path());
            AppError::unauthorized("Your session expired. Please sign in again.")
        })?;
        // First request from this account: remember the browser's timezone (falls back to UTC).
        let tz = parts.headers.get("x-timezone").and_then(|v| v.to_str().ok()).filter(|t| billing::parse_tz(t).is_some()).unwrap_or("UTC");
        db::ensure_user(&state.pool, &id, tz).await?;
        Ok(AuthUser { id })
    }
}

// ---------- handlers ----------

pub async fn me(State(s): State<AppState>, user: AuthUser) -> Result<Json<Value>, AppError> {
    let q = billing::quota(&s.pool, &user.id).await?;
    Ok(Json(json!({ "user": { "id": user.id }, "subscription": q })))
}

#[derive(Deserialize)]
pub struct SetTz { timezone: String }

/// Lets the user correct their timezone. Changes are limited to once per 30 days (the first one is free) so the
/// daily reset cannot be gamed by hopping between timezones.
pub async fn set_timezone(State(s): State<AppState>, user: AuthUser, Json(req): Json<SetTz>) -> Result<Json<Value>, AppError> {
    if billing::parse_tz(&req.timezone).is_none() { return Err(AppError::bad_request("Unknown timezone.")); }
    let (cur, changed): (String, i64) = sqlx::query_as("SELECT tz, tz_changed_at FROM users WHERE id = ?").bind(&user.id).fetch_one(&s.pool).await?;
    if cur == req.timezone {
        return Ok(Json(json!({ "ok": true, "changed": false })));
    }
    let t = now();
    if changed != 0 && t - changed < TZ_COOLDOWN_SECS {
        return Err(AppError(StatusCode::TOO_MANY_REQUESTS, format!("You can change your timezone again in {} day(s).", (TZ_COOLDOWN_SECS - (t - changed)) / 86_400 + 1)));
    }
    sqlx::query("UPDATE users SET tz = ?, tz_changed_at = ? WHERE id = ?").bind(&req.timezone).bind(t).bind(&user.id).execute(&s.pool).await?;
    Ok(Json(json!({ "ok": true, "changed": true })))
}

/// Deletes the account in Clerk, then erases all local data. If Clerk fails we stop, so nothing is half-deleted.
pub async fn delete_account(State(s): State<AppState>, user: AuthUser) -> Result<Json<Value>, AppError> {
    if !s.limiter.allow(&format!("del:{}", user.id), 5, Duration::from_secs(600)) {
        return Err(AppError::too_many("Too many attempts. Please wait."));
    }
    if let Err(e) = s.clerk.delete_user(&user.id).await {
        tracing::error!("Clerk delete failed for {}: {e:#}", user.id);
        return Err(AppError(StatusCode::BAD_GATEWAY, "Could not delete your account right now. Please try again.".into()));
    }
    db::erase_user(&s.pool, &user.id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct Event { #[serde(rename = "type")] kind: String, data: Value }

/// Clerk (Svix) webhook. Handles `user.deleted` by erasing that user's data; other events are acknowledged and ignored.
pub async fn webhook(State(s): State<AppState>, headers: HeaderMap, body: Bytes) -> Result<Json<Value>, AppError> {
    let Some(secret) = s.clerk.webhook_secret.as_deref() else {
        return Err(AppError(StatusCode::SERVICE_UNAVAILABLE, "Webhook is not configured.".into()));
    };
    let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !verify_svix(secret, h("svix-id"), h("svix-timestamp"), &body, h("svix-signature"), now()) {
        return Err(AppError::unauthorized("Invalid signature."));
    }
    let ev: Event = serde_json::from_slice(&body).map_err(|_| AppError::bad_request("Bad payload."))?;
    if ev.kind == "user.deleted" {
        if let Some(id) = ev.data.get("id").and_then(Value::as_str) {
            db::erase_user(&s.pool, id).await?;
            tracing::info!("erased data for deleted Clerk user {id}");
        }
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    // Throw-away 2048-bit RSA key pair, generated only for these tests.
    const PRIV: &str = include_str!("../testdata/test_rsa_private.pem");
    const PUB: &str = include_str!("../testdata/test_rsa_public.pem");
    const ISS: &str = "https://clerk.test.example";

    fn clerk() -> Clerk {
        let c = Clerk::for_test(ISS, &["http://localhost:3000"], None);
        c.insert_key("kid1", DecodingKey::from_rsa_pem(PUB.as_bytes()).unwrap());
        c
    }

    fn token(kid: &str, claims: serde_json::Value) -> String {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = Some(kid.into());
        encode(&h, &claims, &EncodingKey::from_rsa_pem(PRIV.as_bytes()).unwrap()).unwrap()
    }

    fn good() -> serde_json::Value {
        let t = now();
        json!({"sub": "user_123", "iss": ISS, "azp": "http://localhost:3000", "exp": t + 60, "nbf": t - 5, "iat": t - 5, "sid": "sess_1"})
    }

    #[tokio::test]
    async fn accepts_valid_token() {
        assert_eq!(clerk().verify(&token("kid1", good())).await.unwrap(), "user_123");
    }

    #[tokio::test]
    async fn rejects_bad_tokens() {
        let c = clerk();
        let t = now();
        let mut expired = good(); expired["exp"] = json!(t - 60);
        assert!(c.verify(&token("kid1", expired)).await.is_err(), "expired");
        let mut early = good(); early["nbf"] = json!(t + 3600);
        assert!(c.verify(&token("kid1", early)).await.is_err(), "not yet valid");
        let mut other_iss = good(); other_iss["iss"] = json!("https://evil.example");
        assert!(c.verify(&token("kid1", other_iss)).await.is_err(), "wrong issuer");
        let mut other_azp = good(); other_azp["azp"] = json!("https://evil.example");
        assert!(c.verify(&token("kid1", other_azp)).await.is_err(), "wrong authorized party");
        let mut no_sub = good(); no_sub.as_object_mut().unwrap().remove("sub");
        assert!(c.verify(&token("kid1", no_sub)).await.is_err(), "no subject");
        assert!(c.verify(&token("unknown-kid", good())).await.is_err(), "unknown key id");
        let ok = token("kid1", good());
        assert!(c.verify(&format!("{ok}x")).await.is_err(), "tampered signature");
        assert!(c.verify("not.a.jwt").await.is_err());
        // alg=none / HS256 confusion must never be accepted
        let hs = encode(&Header::new(Algorithm::HS256), &good(), &EncodingKey::from_secret(PUB.as_bytes()).unwrap()).unwrap();
        assert!(c.verify(&hs).await.is_err(), "HS256 token signed with the public key");
    }

    #[tokio::test]
    async fn azp_is_optional_when_absent() {
        let mut claims = good();
        claims.as_object_mut().unwrap().remove("azp");
        assert!(clerk().verify(&token("kid1", claims)).await.is_ok());
    }

    fn sign(secret: &[u8], id: &str, ts: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(format!("{id}.{ts}.").as_bytes());
        mac.update(body);
        format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn svix_signatures() {
        let secret = b"super-secret-bytes-0123456789";
        let body = br#"{"type":"user.deleted","data":{"id":"user_1"}}"#;
        let now_ts = 1_800_000_000;
        let ts = now_ts.to_string();
        let sig = sign(secret, "msg_1", &ts, body);
        assert!(verify_svix(secret, "msg_1", &ts, body, &sig, now_ts));
        assert!(verify_svix(secret, "msg_1", &ts, body, &format!("v1,AAAA {sig}"), now_ts), "any listed signature may match");
        assert!(!verify_svix(secret, "msg_2", &ts, body, &sig, now_ts), "id is signed");
        assert!(!verify_svix(secret, "msg_1", &ts, b"tampered", &sig, now_ts));
        assert!(!verify_svix(b"other-secret-bytes-0123456789", "msg_1", &ts, body, &sig, now_ts));
        assert!(!verify_svix(secret, "msg_1", &ts, body, &sig, now_ts + 301), "replay outside tolerance");
        assert!(!verify_svix(secret, "msg_1", &ts, body, "", now_ts));
        assert!(!verify_svix(secret, "msg_1", "abc", body, &sig, now_ts));
    }

    #[test]
    fn whsec_parsing() {
        assert_eq!(parse_whsec("whsec_aGVsbG8=").unwrap(), b"hello");
        assert!(parse_whsec("whsec_***").is_err());
    }

    #[test]
    fn url_encoding_of_user_ids() {
        assert_eq!(urlencode("user_2abc-XYZ"), "user_2abc-XYZ");
        assert_eq!(urlencode("a/b?c"), "a%2Fb%3Fc");
    }
}
