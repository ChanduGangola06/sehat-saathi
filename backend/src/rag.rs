use crate::{db, openai::OpenAi};
use anyhow::Result;
use serde::Serialize;
use sqlx::SqlitePool;
use std::path::Path;

#[derive(Serialize, Debug)]
pub struct Hit {
    pub source: String,
    pub text: String,
    pub score: f32,
}

/// Split into ~`max` char chunks on paragraph boundaries (long paragraphs are hard-split).
pub fn chunk_text(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if !cur.trim().is_empty() {
            out.push(cur.trim().to_string());
        }
        cur.clear();
    };
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        let n = para.chars().count();
        if n > max {
            flush(&mut cur, &mut out);
            let chars: Vec<char> = para.chars().collect();
            for w in chars.chunks(max) {
                out.push(w.iter().collect());
            }
            continue;
        }
        if cur.chars().count() + n + 2 > max {
            flush(&mut cur, &mut out);
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
    }
    flush(&mut cur, &mut out);
    out
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
}

/// Embed and store a document. `user = None` stores it in the shared knowledge base.
/// Returns (chunks stored, embedding tokens used).
pub async fn ingest_text(pool: &SqlitePool, ai: &OpenAi, user: Option<&str>, source: &str, text: &str) -> Result<(usize, u64)> {
    let chunks = chunk_text(text, 900);
    let mut tokens = 0;
    for batch in chunks.chunks(32) {
        let (embs, t) = ai.embed(batch).await?;
        tokens += t;
        for (c, e) in batch.iter().zip(embs) {
            db::insert_chunk(pool, user, source, c, &e).await?;
        }
    }
    Ok((chunks.len(), tokens))
}

/// Load every .md file in `dir` into the shared knowledge base (skips files already ingested).
pub async fn ingest_dir(pool: &SqlitePool, ai: &OpenAi, dir: &Path) -> Result<()> {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(r) => r,
        Err(_) => { tracing::warn!("knowledge dir {dir:?} not found"); return Ok(()); }
    };
    while let Some(entry) = rd.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
        let name = entry.file_name().to_string_lossy().to_string();
        if db::count_chunks(pool, None, &name).await? > 0 { continue; }
        let text = tokio::fs::read_to_string(&path).await?;
        let (n, _) = ingest_text(pool, ai, None, &name, &text).await?;
        tracing::info!("ingested {name}: {n} chunks");
    }
    Ok(())
}

pub async fn search(pool: &SqlitePool, ai: &OpenAi, user: Option<&str>, query: &str, k: usize, tokens: &mut u64) -> Result<Vec<Hit>> {
    let chunks = db::load_chunks(pool, user).await?;
    if chunks.is_empty() { return Ok(vec![]); }
    let (mut embs, t) = ai.embed(&[query.to_string()]).await?;
    *tokens += t;
    let q = embs.remove(0);
    // Chunks embedded by a different provider/model have a different size and cannot be compared; skip them.
    let mut hits: Vec<Hit> = chunks.into_iter().filter(|c| c.embedding.len() == q.len())
        .map(|c| Hit { score: cosine(&q, &c.embedding), source: c.source, text: c.text })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(k);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_paragraphs_and_size() {
        let text = "aaa\n\nbbb\n\nccc";
        assert_eq!(chunk_text(text, 100), vec!["aaa\n\nbbb\n\nccc"]);
        assert_eq!(chunk_text(text, 8), vec!["aaa\n\nbbb", "ccc"]);
        let long = "x".repeat(25);
        assert_eq!(chunk_text(&long, 10).len(), 3);
        assert!(chunk_text("  \n\n ", 10).is_empty());
    }

    #[test]
    fn hindi_text_is_not_split_mid_char() {
        let long = "स्वास्थ्य".repeat(10);
        for c in chunk_text(&long, 7) { assert!(c.chars().count() <= 7); }
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }
}
