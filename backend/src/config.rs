use anyhow::{Result, bail};

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn env_bool(name: &str) -> bool {
    std::env::var(name).map(|v| v == "true" || v == "1").unwrap_or(false)
}

fn env_num(name: &str, default: i64) -> i64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

#[derive(Clone)]
pub struct Config {
    pub production: bool,
    pub frontend_origin: String,
    pub webhook_secret: Option<String>,
    pub dev_billing: bool,
    /// Per-user cap on AI requests (chat, voice, documents) per minute, on top of the token quota.
    pub ai_rate_per_min: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let production = std::env::var("APP_ENV").map(|v| v == "production").unwrap_or(false);
        let dev_billing = env_bool("BILLING_DEV_MODE");
        if production && dev_billing {
            bail!("BILLING_DEV_MODE must not be enabled in production");
        }
        Ok(Self {
            production,
            frontend_origin: std::env::var("FRONTEND_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()),
            webhook_secret: std::env::var("BILLING_WEBHOOK_SECRET").ok().filter(|s| s.len() >= 16),
            dev_billing,
            ai_rate_per_min: env_num("AI_RATE_PER_MIN", 20).max(1) as usize,
        })
    }
}
