/// Claude usage data fetcher.
///
/// Strategy (matching CodeZeno/Claude-Code-Usage-Monitor):
///   1. Read OAuth token from ~\.claude\.credentials.json
///      JSON path: claudeAiOauth.accessToken  (expiresAt in ms for expiry check)
///   2. Try dedicated usage endpoint first:
///        GET https://api.anthropic.com/api/oauth/usage
///        Authorization: Bearer <token>
///        anthropic-beta: oauth-2025-04-20
///      Response JSON: { five_hour: { utilization, resets_at }, seven_day: { utilization, resets_at } }
///   3. If token expired (expiresAt), refresh via `claude -p .` then retry once.
///   4. Fallback to POST /v1/messages and parse rate-limit headers (same beta header).
use crate::state::WindowUsage;
use chrono::DateTime;
use log::{debug, info, warn};
use reqwest::{Client, header::HeaderMap};
use serde::Deserialize;
use std::path::PathBuf;

const OAUTH_BETA: &str = "oauth-2025-04-20";
const USAGE_URL:    &str = "https://api.anthropic.com/api/oauth/usage";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";

// ─── Credential structs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ClaudeOauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Unix timestamp in **milliseconds** (may be absent in older installs).
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<ClaudeOauth>,
    /// Legacy / API-key installs store the key directly.
    api_key: Option<String>,
}

// ─── Credential loading ──────────────────────────────────────────────────────

fn candidate_paths() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    let base = PathBuf::from(&home).join(".claude");
    let appdata = std::env::var("APPDATA").unwrap_or_default();

    vec![
        base.join(".credentials.json"),
        base.join("credentials.json"),
        PathBuf::from(&appdata).join("Claude").join(".credentials.json"),
        PathBuf::from(&appdata).join("Claude").join("credentials.json"),
    ]
}

struct LoadedToken {
    token:      String,
    expires_at: Option<i64>, // ms
    is_api_key: bool,
}

fn load_token() -> Result<LoadedToken, String> {
    let candidates = candidate_paths();
    for p in &candidates {
        info!("[claude] candidate: {} exists={}", p.display(), p.exists());
    }

    let (path, data) = candidates
        .into_iter()
        .find_map(|p| std::fs::read_to_string(&p).ok().map(|d| (p, d)))
        .ok_or_else(|| "credentials file not found".to_string())?;

    info!("[claude] reading from {}", path.display());
    debug!("[claude] raw json prefix: {}", &data[..data.len().min(200)]);

    let creds: ClaudeCredentials = serde_json::from_str(&data)
        .map_err(|e| format!("parse error in {}: {}", path.display(), e))?;

    if let Some(oauth) = creds.oauth {
        if !oauth.access_token.is_empty() {
            info!(
                "[claude] using claudeAiOauth.accessToken ({}…)  expires_at={:?}",
                &oauth.access_token[..oauth.access_token.len().min(20)],
                oauth.expires_at
            );
            return Ok(LoadedToken {
                token: oauth.access_token,
                expires_at: oauth.expires_at,
                is_api_key: false,
            });
        }
    }
    if let Some(key) = creds.api_key {
        if !key.is_empty() {
            info!("[claude] using api_key field");
            return Ok(LoadedToken { token: key, expires_at: None, is_api_key: true });
        }
    }
    Err("No token found — claudeAiOauth.accessToken and api_key both empty/missing".to_string())
}

/// Check if the OAuth token is expired (expires_at is in milliseconds).
fn is_expired(expires_at: Option<i64>) -> bool {
    let Some(exp_ms) = expires_at else { return false };
    let now_ms = chrono::Utc::now().timestamp_millis();
    now_ms >= exp_ms
}

/// Refresh the token by spawning `claude -p .`  (Claude CLI performs OAuth refresh
/// and writes updated credentials to disk).  Returns Ok if the process exited 0.
fn refresh_token_via_cli() -> Result<(), String> {
    info!("[claude] token expired — attempting refresh via `claude -p .`");
    let status = std::process::Command::new("claude")
        .args(["-p", "."])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("failed to spawn claude CLI: {}", e))?;
    if status.success() {
        info!("[claude] CLI refresh succeeded");
        Ok(())
    } else {
        Err(format!("claude CLI exited with {}", status))
    }
}

// ─── Usage response structs ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct UsageWindow {
    utilization: Option<f32>,
    resets_at:   Option<String>, // RFC3339
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    five_hour:  Option<UsageWindow>,
    seven_day:  Option<UsageWindow>,
}

fn parse_rfc3339(s: &str) -> Option<DateTime<chrono::Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn normalize_util(v: Option<f32>) -> Option<f32> {
    let x = v?;
    if x > 1.0 {
        Some((x / 100.0).clamp(0.0, 1.0))
    } else {
        Some(x.clamp(0.0, 1.0))
    }
}

// ─── Fallback header helpers ──────────────────────────────────────────────────

fn parse_f32_hdr(headers: &HeaderMap, name: &str) -> Option<f32> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn parse_dt_hdr(headers: &HeaderMap, name: &str) -> Option<DateTime<chrono::Utc>> {
    let s = headers.get(name)?.to_str().ok()?.trim().to_string();
    debug!("[claude] parse_dt_hdr '{}' = '{}'", name, s);
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Some(dt.with_timezone(&chrono::Utc));
    }
    if let Ok(ts) = s.parse::<i64>() {
        return DateTime::from_timestamp(ts, 0);
    }
    warn!("[claude] could not parse reset header: '{}'", s);
    None
}

// ─── Primary: dedicated OAuth usage endpoint ──────────────────────────────────

async fn fetch_usage_endpoint(client: &Client, token: &str) -> Result<WindowUsage, String> {
    info!("[claude] GET {}", USAGE_URL);
    let resp = client
        .get(USAGE_URL)
        .header("authorization", format!("Bearer {}", token))
        .header("anthropic-beta", OAUTH_BETA)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let status = resp.status();
    info!("[claude] usage endpoint status: {}", status);

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        warn!("[claude] usage endpoint error body: {}", &body[..body.len().min(300)]);
        return Err(format!("usage endpoint {}: {}", status, &body[..body.len().min(200)]));
    }

    let ur: UsageResponse = resp
        .json()
        .await
        .map_err(|e| format!("JSON parse error: {}", e))?;

    debug!("[claude] usage response: {:?}", ur);

    let usage = WindowUsage {
        utilization_5h: normalize_util(ur.five_hour.as_ref().and_then(|w| w.utilization)),
        utilization_7d: normalize_util(ur.seven_day.as_ref().and_then(|w| w.utilization)),
        reset_5h: ur.five_hour.as_ref().and_then(|w| w.resets_at.as_deref()).and_then(parse_rfc3339),
        reset_7d: ur.seven_day.as_ref().and_then(|w| w.resets_at.as_deref()).and_then(parse_rfc3339),
    };
    info!(
        "[claude] usage endpoint → 5h={:?}  7d={:?}",
        usage.utilization_5h, usage.utilization_7d
    );
    Ok(usage)
}

// ─── Fallback: POST /v1/messages, read rate-limit headers ────────────────────

const PROBE_BODY: &str = r#"{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"."}]}"#;

async fn fetch_via_messages(client: &Client, token: &str, is_api_key: bool) -> Result<WindowUsage, String> {
    info!("[claude] POST {} (fallback)", MESSAGES_URL);

    let req = client
        .post(MESSAGES_URL)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");

    let req = if is_api_key {
        req.header("x-api-key", token)
    } else {
        req.header("authorization", format!("Bearer {}", token))
           .header("anthropic-beta", OAUTH_BETA)
    };

    let resp = req
        .body(PROBE_BODY)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {}", e))?;

    let status = resp.status();
    info!("[claude] messages fallback status: {}", status);

    let headers = resp.headers().clone();

    for (k, v) in headers.iter() {
        if k.as_str().starts_with("anthropic-ratelimit") {
            debug!("[claude] hdr {} = {}", k, v.to_str().unwrap_or("?"));
        }
    }

    if !status.is_success() && status.as_u16() != 429 {
        let body = resp.text().await.unwrap_or_default();
        warn!("[claude] messages body: {}", &body[..body.len().min(300)]);
        if !headers.contains_key("anthropic-ratelimit-unified-5h-utilization") {
            return Err(format!("Claude API {}: {}", status, &body[..body.len().min(200)]));
        }
    }

    let usage = WindowUsage {
        utilization_5h: normalize_util(parse_f32_hdr(&headers, "anthropic-ratelimit-unified-5h-utilization")),
        utilization_7d: normalize_util(parse_f32_hdr(&headers, "anthropic-ratelimit-unified-7d-utilization")),
        reset_5h: parse_dt_hdr(&headers, "anthropic-ratelimit-unified-5h-reset"),
        reset_7d: parse_dt_hdr(&headers, "anthropic-ratelimit-unified-7d-reset"),
    };
    info!(
        "[claude] messages fallback → 5h={:?}  7d={:?}",
        usage.utilization_5h, usage.utilization_7d
    );
    Ok(usage)
}

// ─── Public entry point ───────────────────────────────────────────────────────

pub async fn fetch(client: &Client) -> Result<WindowUsage, String> {
    let mut loaded = load_token()?;

    // If the OAuth token is expired, try to refresh via the Claude CLI once.
    if !loaded.is_api_key && is_expired(loaded.expires_at) {
        if refresh_token_via_cli().is_ok() {
            // Re-read the freshly written credentials.
            match load_token() {
                Ok(t) => { loaded = t; }
                Err(e) => warn!("[claude] re-read after refresh failed: {}", e),
            }
        }
    }

    // OAuth tokens → try the dedicated usage endpoint first.
    if !loaded.is_api_key {
        match fetch_usage_endpoint(client, &loaded.token).await {
            Ok(u) => return Ok(u),
            Err(e) => warn!("[claude] usage endpoint failed, falling back to messages: {}", e),
        }
    }

    // API-key path (or usage endpoint failed) → POST /v1/messages fallback.
    fetch_via_messages(client, &loaded.token, loaded.is_api_key).await
}
