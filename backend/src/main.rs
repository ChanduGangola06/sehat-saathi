mod agent;
mod billing;
mod clerk;
mod config;
mod db;
mod error;
mod live;
mod openai;
mod ratelimit;
mod rag;
mod routes;
mod safety;
mod state;

use axum::http::{HeaderName, HeaderValue, Method, header};
use state::AppState;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tower_http::{cors::CorsLayer, set_header::SetResponseHeaderLayer, trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer}};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "sehat_saathi=info,tower_http=info".into()))
        .init();

    let cfg = Arc::new(config::Config::from_env()?);
    if cfg.dev_billing {
        tracing::warn!("BILLING_DEV_MODE is ON: users can 'pay' without paying. Never use this in production.");
    }
    let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://healthbot.db".into());
    let pool = db::init(&db_url).await?;
    billing::seed_plans(&pool).await?;
    let ai = Arc::new(openai::OpenAi::from_env());
    if !ai.has_key() {
        tracing::warn!("no AI provider configured: chat and RAG calls will fail");
    }
    let clerk = Arc::new(clerk::Clerk::from_env(cfg.production, &cfg.frontend_origin)?);
    let state = AppState { pool: pool.clone(), ai: ai.clone(), cfg: cfg.clone(), limiter: Arc::new(ratelimit::RateLimiter::default()), clerk };

    // Index the shared knowledge base in the background so startup is instant.
    if ai.has_key() {
        let dir = PathBuf::from(std::env::var("KNOWLEDGE_DIR").unwrap_or_else(|_| "knowledge".into()));
        tokio::spawn(async move {
            if let Err(e) = rag::ingest_dir(&pool, &ai, &dir).await {
                tracing::error!("knowledge ingestion failed: {e:#}");
            }
        });
    }

    // Auth is a Bearer token (no cookies), so CORS needs no credentials; keep origin and headers explicit anyway.
    let cors = CorsLayer::new()
        .allow_origin(cfg.frontend_origin.parse::<HeaderValue>()?)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::PUT, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, HeaderName::from_static("x-timezone")]);

    let mut app = routes::router(state)
        .layer(cors)
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .layer(TraceLayer::new_for_http()
            .make_span_with(DefaultMakeSpan::new().level(tracing::Level::INFO))
            .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)));
    if cfg.production {
        app = app.layer(SetResponseHeaderLayer::overriding(
            header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=63072000; includeSubDomains")));
    }

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    tracing::info!("sehat-saathi listening on :{port} (env: {})", if cfg.production { "production" } else { "development" });
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.ok(); })
        .await?;
    Ok(())
}
