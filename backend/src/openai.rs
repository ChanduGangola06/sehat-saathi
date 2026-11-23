//! AI client with automatic fallback between OpenAI-compatible providers.
//!
//! Providers are tried in the order of `AI_PROVIDERS` (default `gemini,ollama,openai`). A provider is used only if it is
//! configured: Gemini and OpenAI need their API key, Ollama (local, free) needs nothing. When a call fails (quota,
//! rate limit, network, server down) the next provider is tried, and a warning is logged.
//!
//! Voice does not go through this file: it is a direct speech-to-speech session with Gemini Live (see `live.rs`).

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
struct Provider {
    name: &'static str,
    base: String,
    key: Option<String>,
    chat_model: String,
    embed_model: String,
}

/// Settings for the speech-to-speech (Gemini Live) session.
#[derive(Clone)]
pub struct LiveCfg {
    pub key: String,
    pub model: String,
    pub voice: String,
}

#[derive(Clone)]
pub struct OpenAi {
    live: Option<LiveCfg>,
    http: reqwest::Client,
    providers: Vec<Provider>,
    /// Name of the first provider, shown by /api/health.
    pub chat_model: String,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}
fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl OpenAi {
    pub fn from_env() -> Self {
        let order = env_or("AI_PROVIDERS", "gemini,ollama,openai");
        let mut providers = Vec::new();
        for name in order.split(',').map(|s| s.trim().to_lowercase()) {
            let p = match name.as_str() {
                "gemini" => env_key("GEMINI_API_KEY").map(|key| Provider {
                    name: "gemini",
                    base: env_or("GEMINI_BASE_URL", "https://generativelanguage.googleapis.com/v1beta/openai"),
                    key: Some(key),
                    chat_model: env_or("GEMINI_CHAT_MODEL", "gemini-flash-latest"),
                    embed_model: env_or("GEMINI_EMBED_MODEL", "gemini-embedding-001"),
                }),
                "ollama" => Some(Provider {
                    name: "ollama",
                    base: env_or("OLLAMA_BASE_URL", "http://localhost:11434/v1"),
                    key: None,
                    chat_model: env_or("OLLAMA_CHAT_MODEL", "llama3.1"),
                    embed_model: env_or("OLLAMA_EMBED_MODEL", "nomic-embed-text"),
                }),
                "openai" => env_key("OPENAI_API_KEY").map(|key| Provider {
                    name: "openai",
                    base: env_or("OPENAI_BASE_URL", "https://api.openai.com/v1"),
                    key: Some(key),
                    chat_model: env_or("CHAT_MODEL", "gpt-4o-mini"),
                    embed_model: env_or("EMBED_MODEL", "text-embedding-3-small"),
                }),
                other => { tracing::warn!("unknown AI provider {other:?} in AI_PROVIDERS, ignored"); None }
            };
            if let Some(p) = p {
                if !providers.iter().any(|q: &Provider| q.name == p.name) { providers.push(p); }
            }
        }
        for p in &providers {
            tracing::info!("AI provider ready: {} (chat model {})", p.name, p.chat_model);
        }
        let chat_model = providers.first().map(|p| format!("{}:{}", p.name, p.chat_model)).unwrap_or_default();
        let live = env_key("GEMINI_API_KEY").map(|key| LiveCfg {
            key,
            model: env_or("GEMINI_LIVE_MODEL", "gemini-2.5-flash-native-audio-latest"),
            voice: env_or("GEMINI_LIVE_VOICE", "Aoede"),
        });
        if let Some(l) = &live { tracing::info!("voice: Gemini Live ready (model {}, voice {})", l.model, l.voice); }
        else { tracing::warn!("GEMINI_API_KEY is not set: the voice conversation is unavailable"); }
        Self {
            live,
            http: reqwest::Client::builder().timeout(Duration::from_secs(120)).build().expect("http client"),
            providers,
            chat_model,
        }
    }

    pub fn live(&self) -> Option<&LiveCfg> {
        self.live.as_ref()
    }

    pub fn has_key(&self) -> bool {
        !self.providers.is_empty()
    }

    pub fn provider_names(&self) -> Vec<&'static str> {
        self.providers.iter().map(|p| p.name).collect()
    }

    async fn check(name: &str, resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("{name} API error {status}: {body}")
    }

    async fn post_json(&self, p: &Provider, path: &str, body: &Value) -> Result<Value> {
        let mut req = self.http.post(format!("{}{path}", p.base)).json(body);
        if let Some(k) = &p.key { req = req.bearer_auth(k); }
        let resp = req.send().await.with_context(|| format!("{} is unreachable", p.name))?;
        Ok(Self::check(p.name, resp).await?.json().await?)
    }

    /// One chat-completions call; returns the assistant `message` object (may contain `tool_calls`).
    pub async fn chat(&self, messages: &[Value], tools: Option<&Value>) -> Result<(Value, u64)> {
        let mut last: Option<anyhow::Error> = None;
        for p in &self.providers {
            let mut body = json!({ "model": p.chat_model, "messages": messages });
            if let Some(t) = tools { body["tools"] = t.clone(); }
            match self.post_json(p, "/chat/completions", &body).await {
                Ok(v) => {
                    if last.is_some() { tracing::info!("chat answered by fallback provider {}", p.name); }
                    return Ok((v["choices"][0]["message"].clone(), v["usage"]["total_tokens"].as_u64().unwrap_or(0)));
                }
                Err(e) => { tracing::warn!("chat via {} failed, trying next provider: {e:#}", p.name); last = Some(e); }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no AI provider is configured")))
    }

    pub async fn embed(&self, inputs: &[String]) -> Result<(Vec<Vec<f32>>, u64)> {
        let mut last: Option<anyhow::Error> = None;
        for p in &self.providers {
            match self.embed_with(p, inputs).await {
                Ok(r) => return Ok(r),
                Err(e) => { tracing::warn!("embeddings via {} failed, trying next provider: {e:#}", p.name); last = Some(e); }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no AI provider is configured")))
    }

    async fn embed_with(&self, p: &Provider, inputs: &[String]) -> Result<(Vec<Vec<f32>>, u64)> {
        let v = self.post_json(p, "/embeddings", &json!({ "model": p.embed_model, "input": inputs })).await?;
        let data = v["data"].as_array().context("bad embeddings response")?;
        let vecs = data.iter()
            .map(|d| {
                d["embedding"].as_array().context("missing embedding")
                    .map(|a| a.iter().filter_map(|x| x.as_f64()).map(|x| x as f32).collect())
            })
            .collect::<Result<Vec<Vec<f32>>>>()?;
        Ok((vecs, v["usage"]["total_tokens"].as_u64().unwrap_or(0)))
    }
}
