use crate::{clerk::Clerk, config::Config, openai::OpenAi, ratelimit::RateLimiter};
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub ai: Arc<OpenAi>,
    pub cfg: Arc<Config>,
    pub limiter: Arc<RateLimiter>,
    pub clerk: Arc<Clerk>,
}
