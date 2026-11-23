//! Speech-to-speech conversation (like the Gemini mobile app): the browser streams microphone audio to this server,
//! the server relays it to Gemini Live, and the model's spoken answer streams straight back. Nothing is converted to
//! text on the way: the audio goes in and the audio comes out. Gemini also sends captions (transcripts), which we show
//! on screen, save to the chat history and run through the same deterministic emergency check as typed chat.
//!
//! Wire protocol on `/api/live` (WebSocket):
//!   client -> server  text   `{"type":"auth","token":"<Clerk JWT>","lang":"en|hi"}`  (must be the first message)
//!   client -> server  binary 16-bit PCM, 16 kHz, mono microphone audio
//!   client -> server  text   `{"type":"audio_end"}` (the user stopped talking: the client stops streaming silence)
//!   client -> server  text   `{"type":"end"}`
//!   server -> client  text   `{"type":"ready" | "user_text" | "assistant_text" | "interrupted" | "turn_complete" | "tool" | "emergency" | "error" ...}`
//!   server -> client  binary 16-bit PCM, 24 kHz, mono model audio
//!
//! The API key stays on the server. The Clerk token is sent in the first message, not in the URL, so it never reaches logs.

use crate::{agent, billing, db, safety, state::AppState};
use anyhow::{Context, Result, anyhow};
use axum::{
    extract::{State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message as Up};

const MAX_SESSION: Duration = Duration::from_secs(10 * 60);
/// A session with no speech from either side for this long is closed, so a forgotten open mic costs nothing.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const GEMINI_WS: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

type ClientTx = SplitSink<WebSocket, Message>;
type UpTx = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Up>;

pub async fn ws_handler(State(s): State<AppState>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    // Browsers always send Origin on WebSocket handshakes; refuse other websites.
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != s.cfg.frontend_origin {
            tracing::warn!("live: refused WebSocket from origin {origin}");
            return (StatusCode::FORBIDDEN, "forbidden").into_response();
        }
    }
    ws.max_message_size(256 * 1024).on_upgrade(move |sock| async move {
        if let Err(e) = session(s, sock).await { tracing::warn!("live: session ended with error: {e:#}"); }
    })
}

async fn send_json(tx: &mut ClientTx, v: Value) {
    let _ = tx.send(Message::Text(v.to_string().into())).await;
}

#[derive(Default)]
struct Turn {
    user_text: String,
    assistant_text: String,
    emergency: bool,
}

struct Session {
    s: AppState,
    user: String,
    lang: String,
    cl: ClientTx,
    up: UpTx,
    turn: Turn,
    /// Tokens used by tools such as knowledge search (embeddings). The voice conversation itself is billed in minutes.
    pending_tokens: u64,
    /// Last time either side said something.
    activity: Instant,
}

/// Gemini sometimes repeats a caption twice in a row; collapse an exact doubling.
fn dedupe(t: &str) -> String {
    let t = t.trim();
    let n = t.len();
    if n >= 8 && n % 2 == 0 && t.is_char_boundary(n / 2) && t[..n / 2] == t[n / 2..] { return t[..n / 2].to_string(); }
    t.to_string()
}

async fn session(s: AppState, client: WebSocket) -> Result<()> {
    let (mut cl, mut cl_rx) = client.split();

    // 1. Authenticate with the first message.
    let first = tokio::time::timeout(AUTH_TIMEOUT, cl_rx.next()).await.ok().flatten();
    let Some(Ok(Message::Text(t))) = first else { return Ok(()) };
    let v: Value = serde_json::from_str(&t).unwrap_or_default();
    let (token, lang) = (v["token"].as_str().unwrap_or_default(), if v["lang"] == "hi" { "hi" } else { "en" });
    if v["type"] != "auth" || token.is_empty() {
        send_json(&mut cl, json!({ "type": "error", "code": "unauthorized", "message": "Please sign in." })).await;
        return Ok(());
    }
    let user = match s.clerk.verify(token).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("live: auth rejected: {e:#}");
            send_json(&mut cl, json!({ "type": "error", "code": "unauthorized", "message": "Your session expired. Please sign in again." })).await;
            return Ok(());
        }
    };
    db::ensure_user(&s.pool, &user, "UTC").await?;
    if !s.limiter.allow(&format!("live:{user}"), 6, Duration::from_secs(60)) {
        send_json(&mut cl, json!({ "type": "error", "code": "rate_limited", "message": "Too many voice sessions. Please wait a moment." })).await;
        return Ok(());
    }
    let voice_left = billing::quota(&s.pool, &user).await?.voice_remaining_secs;
    if voice_left <= 0 {
        send_json(&mut cl, json!({ "type": "error", "code": "voice_quota_exceeded", "message": "You have used today's voice minutes. Upgrade your plan or type your question." })).await;
        return Ok(());
    }
    let Some(live) = s.ai.live().cloned() else {
        send_json(&mut cl, json!({ "type": "error", "code": "voice_unavailable", "message": "Voice is not configured on this server." })).await;
        return Ok(());
    };

    // 2. Connect to Gemini Live and configure the session.
    let (up_ws, _) = connect_async(format!("{GEMINI_WS}?key={}", live.key)).await
        .map_err(|e| anyhow!("could not reach Gemini Live: {}", e.to_string().replace(&live.key, "***")))?;
    let (mut up, mut up_rx) = up_ws.split();
    let setup = json!({ "setup": {
        "model": format!("models/{}", live.model),
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": live.voice } } }
        },
        "systemInstruction": { "parts": [{ "text": agent::voice_system_prompt(lang) }] },
        "tools": agent::voice_tools(),
        // Long conversations keep a rolling window instead of growing (and costing) without limit.
        "contextWindowCompression": { "slidingWindow": {} },
        "inputAudioTranscription": {},
        "outputAudioTranscription": {}
    }});
    up.send(Up::Text(setup.to_string().into())).await.context("sending setup")?;
    loop {
        let m = tokio::time::timeout(Duration::from_secs(15), up_rx.next()).await.context("Gemini Live did not answer")?;
        let Some(Ok(m)) = m else { return Err(anyhow!("Gemini Live closed during setup")) };
        if let Some(v) = parse_up(m) {
            if v.get("setupComplete").is_some() { break; }
            if v.get("error").is_some() { return Err(anyhow!("Gemini Live setup error: {}", v["error"])); }
        }
    }
    tracing::info!("live: session started for user {user} ({lang})");
    send_json(&mut cl, json!({ "type": "ready" })).await;

    let mut sess = Session { s, user, lang: lang.to_string(), cl, up, turn: Turn::default(), pending_tokens: 0, activity: Instant::now() };
    // The session ends at the earlier of the hard cap and the user's remaining voice minutes for today.
    let limit = MAX_SESSION.min(Duration::from_secs(voice_left as u64));
    let quota_limited = limit < MAX_SESSION;
    let started = Instant::now();
    let deadline = tokio::time::sleep(limit);
    tokio::pin!(deadline);
    let mut idle_check = tokio::time::interval(Duration::from_secs(5));
    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = &mut deadline => {
                    let (code, msg) = if quota_limited { ("voice_limit", "You have used today's voice minutes.") } else { ("time_limit", "Voice session time limit reached.") };
                    send_json(&mut sess.cl, json!({ "type": "error", "code": code, "message": msg })).await;
                    return Ok(());
                }
                _ = idle_check.tick() => {
                    if sess.activity.elapsed() > IDLE_TIMEOUT {
                        send_json(&mut sess.cl, json!({ "type": "error", "code": "idle", "message": "Voice ended because it was quiet for a while." })).await;
                        return Ok(());
                    }
                }
                m = cl_rx.next() => match m {
                    Some(Ok(Message::Binary(pcm))) => {
                        sess.activity = Instant::now();
                        let msg = json!({ "realtimeInput": { "audio": { "data": B64.encode(&pcm), "mimeType": "audio/pcm;rate=16000" } } });
                        sess.up.send(Up::Text(msg.to_string().into())).await?;
                    }
                    Some(Ok(Message::Text(t))) => {
                        match serde_json::from_str::<Value>(&t).ok().and_then(|v| v["type"].as_str().map(str::to_string)).as_deref() {
                            Some("end") => return Ok(()),
                            // The client stops streaming silence; tell Gemini the audio paused so it answers right away.
                            Some("audio_end") => { sess.up.send(Up::Text(json!({ "realtimeInput": { "audioStreamEnd": true } }).to_string().into())).await?; }
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return Ok(()),
                    _ => {}
                },
                m = up_rx.next() => match m {
                    Some(Ok(m)) => { if let Some(v) = parse_up(m) { if !sess.on_server(v).await? { return Ok(()); } } }
                    Some(Err(e)) => return Err(anyhow!("Gemini Live connection error: {e}")),
                    None => {
                        send_json(&mut sess.cl, json!({ "type": "error", "code": "upstream_closed", "message": "The voice connection was closed. Please try again." })).await;
                        return Ok(());
                    }
                },
            }
        }
    }.await;
    sess.finish_turn().await.ok();
    let used = started.elapsed().as_secs().min(voice_left as u64) as i64;
    if let Err(e) = billing::record_voice_seconds(&sess.s.pool, &sess.user, used).await { tracing::error!("live: could not record voice time: {e:#}"); }
    let _ = sess.up.close().await;
    tracing::info!("live: session ended for user {} after {}s", sess.user, used);
    result
}

fn parse_up(m: Up) -> Option<Value> {
    match m {
        Up::Text(t) => serde_json::from_str(&t).ok(),
        Up::Binary(b) => serde_json::from_slice(&b).ok(),
        _ => None,
    }
}

impl Session {
    /// Returns false when the session should end.
    async fn on_server(&mut self, v: Value) -> Result<bool> {
        if let Some(calls) = v["toolCall"]["functionCalls"].as_array() {
            let mut responses = Vec::new();
            for c in calls {
                let name = c["name"].as_str().unwrap_or_default();
                let mut tokens = 0u64;
                let out = match agent::exec_tool(&self.s, &self.user, name, &c["args"], &mut tokens).await {
                    Ok(o) => o,
                    Err(e) => { tracing::warn!("live: tool {name} failed: {e:#}"); json!({ "error": "tool failed" }) }
                };
                self.pending_tokens += tokens;
                send_json(&mut self.cl, json!({ "type": "tool", "name": name })).await;
                responses.push(json!({ "id": c["id"], "name": name, "response": { "output": out } }));
            }
            self.up.send(Up::Text(json!({ "toolResponse": { "functionResponses": responses } }).to_string().into())).await?;
        }
        let sc = &v["serverContent"];
        if sc.is_null() { return Ok(true); }

        if let Some(parts) = sc["modelTurn"]["parts"].as_array() {
            for p in parts {
                if let Some(b64) = p["inlineData"]["data"].as_str() {
                    if let Ok(pcm) = B64.decode(b64) { self.activity = Instant::now(); let _ = self.cl.send(Message::Binary(pcm.into())).await; }
                }
            }
        }
        if let Some(t) = sc["inputTranscription"]["text"].as_str() {
            self.activity = Instant::now();
            self.turn.user_text.push_str(t);
            send_json(&mut self.cl, json!({ "type": "user_text", "text": t })).await;
            self.check_emergency().await?;
        }
        if let Some(t) = sc["outputTranscription"]["text"].as_str() {
            self.turn.assistant_text.push_str(t);
            send_json(&mut self.cl, json!({ "type": "assistant_text", "text": t })).await;
        }
        if sc["interrupted"].as_bool().unwrap_or(false) {
            send_json(&mut self.cl, json!({ "type": "interrupted" })).await;
        }
        if sc["turnComplete"].as_bool().unwrap_or(false) {
            self.finish_turn().await?;
            send_json(&mut self.cl, json!({ "type": "turn_complete" })).await;
        }
        Ok(true)
    }

    /// Same deterministic safety layer as typed chat: emergency phrases get the fixed reply, never improvised advice.
    async fn check_emergency(&mut self) -> Result<()> {
        if self.turn.emergency { return Ok(()); }
        let Some(kind) = safety::detect_emergency(&self.turn.user_text) else { return Ok(()) };
        self.turn.emergency = true;
        let hindi = self.lang == "hi" || safety::has_devanagari(&self.turn.user_text);
        let msg = safety::emergency_message(kind, hindi);
        tracing::warn!("live: emergency phrase detected for user {}", self.user);
        send_json(&mut self.cl, json!({ "type": "emergency", "text": msg })).await;
        let turn = json!({ "clientContent": { "turns": [{ "role": "user", "parts": [{ "text": format!(
            "SYSTEM NOTICE: the user described a possible emergency. Interrupt yourself and say the following message to the user, word for word, calmly, in the same language, and add nothing else:\n{msg}") }] }], "turnComplete": true } });
        self.up.send(Up::Text(turn.to_string().into())).await?;
        Ok(())
    }

    /// Saves the finished turn's captions to the chat history and bills tool tokens (voice time is billed when the session ends).
    async fn finish_turn(&mut self) -> Result<()> {
        let turn = std::mem::take(&mut self.turn);
        let user_text = dedupe(&turn.user_text);
        if !user_text.is_empty() {
            db::save_message(&self.s.pool, &self.user, "user", &user_text).await?;
            if turn.emergency {
                let hindi = self.lang == "hi" || safety::has_devanagari(&user_text);
                if let Some(kind) = safety::detect_emergency(&user_text) {
                    db::save_message(&self.s.pool, &self.user, "assistant", safety::emergency_message(kind, hindi)).await?;
                }
            } else {
                let reply = dedupe(&turn.assistant_text);
                if !reply.is_empty() { db::save_message(&self.s.pool, &self.user, "assistant", &reply).await?; }
            }
        }
        if self.pending_tokens > 0 {
            billing::record_usage(&self.s.pool, &self.user, self.pending_tokens as i64, "voice_tools").await?;
            self.pending_tokens = 0;
        }
        Ok(())
    }
}
