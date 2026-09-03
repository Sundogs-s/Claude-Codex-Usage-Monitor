/// Claude usage data fetcher (v0.4).
///
/// Strategy:
///   1. Read the OAuth token from ~\.claude\.credentials.json
///      (claudeAiOauth.accessToken / expiresAt / rateLimitTier / subscriptionType).
///   2. GET https://api.anthropic.com/api/oauth/usage
///        Authorization: Bearer <token>
///        anthropic-beta: oauth-2025-04-20
///      Windows parsed from the response:
///        five_hour / seven_day            → rows "5h" / "7d"
///        limits[kind == "weekly_scoped"]  → one row per model bucket (e.g. "Fable")
///        extra_usage (only when enabled)  → row "Extra"
///   3. HTTP 429 → `FetchError::RateLimited`. The caller keeps the last-known rows,
///      marks them stale and backs off. There is NO /v1/messages probe fallback for
///      OAuth tokens any more: that probe consumed quota and never carried Fable.
///   4. Expired / rejected token → ask the Claude Code CLI itself through its headless
///      stream-json control protocol (`get_usage`). The CLI refreshes the OAuth token
///      with its own credential locking and answers with the same windows
///      (rate_limits.model_scoped). This monitor never touches the refresh token, so it
///      can never invalidate the Claude Code login.
///   5. API-key credentials (no plan limits) → POST /v1/messages probe headers (5h/7d only).
use crate::state::{ClaudeUsage, UsageRow};
use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use parking_lot::Mutex;
use reqwest::{header::HeaderMap, Client};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;

const OAUTH_BETA: &str = "oauth-2025-04-20";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
/// Treat the token as expiring this long before `expiresAt` so a request never races expiry.
const EXPIRY_MARGIN_MS: i64 = 5 * 60 * 1000;
/// Hard timeout for one headless CLI round-trip.
const CLI_TIMEOUT: Duration = Duration::from_secs(60);
/// Minimum gap between two CLI spawns (the CLI is a full Claude Code process).
const CLI_MIN_GAP: Duration = Duration::from_secs(120);
const CLI_REQUEST_ID: &str = "usage-monitor";

static LAST_CLI_SPAWN: Mutex<Option<Instant>> = Mutex::new(None);

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum FetchError {
    /// The usage endpoint answered 429. Keep last-known data and back off.
    RateLimited,
    /// Credentials are unusable and the CLI could not refresh them.
    Auth(String),
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::RateLimited => write!(f, "usage API rate-limited"),
            FetchError::Auth(s) | FetchError::Other(s) => write!(f, "{}", s),
        }
    }
}

// ─── Credential structs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ClaudeOauth {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// Unix timestamp in **milliseconds** (may be absent in older installs).
    #[serde(rename = "expiresAt")]
    expires_at: Option<i64>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "claudeAiOauth")]
    oauth: Option<ClaudeOauth>,
    /// Legacy / API-key installs store the key directly.
    api_key: Option<String>,
}

// ─── Credential loading ──────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
}

fn candidate_paths() -> Vec<PathBuf> {
    let base = home_dir().join(".claude");
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
    tier:       Option<String>,
}

/// "default_claude_max_5x" + "max" → "Max 5x"; falls back to the subscription type.
fn tier_label(rate_limit_tier: Option<&str>, subscription: Option<&str>) -> Option<String> {
    if let Some(t) = rate_limit_tier {
        let t = t.to_ascii_lowercase();
        if t.contains("max_20x") { return Some("Max 20x".into()); }
        if t.contains("max_5x")  { return Some("Max 5x".into()); }
    }
    subscription.map(|s| match s.to_ascii_lowercase().as_str() {
        "max"        => "Max".to_string(),
        "pro"        => "Pro".to_string(),
        "team"       => "Team".to_string(),
        "enterprise" => "Enterprise".to_string(),
        other        => other.to_string(),
    })
}

fn load_token() -> Result<LoadedToken, FetchError> {
    let (path, data) = candidate_paths()
        .into_iter()
        .find_map(|p| std::fs::read_to_string(&p).ok().map(|d| (p, d)))
        .ok_or_else(|| FetchError::Auth(
            "Claude credentials not found. Open Claude Code once to sign in.".to_string()))?;

    debug!("[claude] reading credentials from {}", path.display());

    let creds: ClaudeCredentials = serde_json::from_str(&data)
        .map_err(|e| FetchError::Other(format!("credentials parse error in {}: {}", path.display(), e)))?;

    if let Some(oauth) = creds.oauth {
        if !oauth.access_token.is_empty() {
            debug!("[claude] using claudeAiOauth.accessToken expires_at={:?} tier={:?}",
                   oauth.expires_at, oauth.rate_limit_tier);
            let tier = tier_label(oauth.rate_limit_tier.as_deref(), oauth.subscription_type.as_deref());
            return Ok(LoadedToken {
                token: oauth.access_token,
                expires_at: oauth.expires_at,
                is_api_key: false,
                tier,
            });
        }
    }
    if let Some(key) = creds.api_key {
        if !key.is_empty() {
            info!("[claude] using api_key field");
            return Ok(LoadedToken { token: key, expires_at: None, is_api_key: true, tier: None });
        }
    }
    Err(FetchError::Auth("No Claude token in credentials file. Open Claude Code once to sign in.".to_string()))
}

/// True when the OAuth token is expired or about to expire (`expires_at` is in ms).
fn is_expiring(expires_at: Option<i64>) -> bool {
    let Some(exp_ms) = expires_at else { return false };
    let now_ms = Utc::now().timestamp_millis();
    now_ms + EXPIRY_MARGIN_MS >= exp_ms
}

// ─── Usage response structs (shared by the endpoint and the CLI's rate_limits) ─

#[derive(Debug, Default, Deserialize)]
struct UsageWindow {
    /// Percent 0–100.
    utilization: Option<f64>,
    resets_at:   Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ModelScope {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LimitScope {
    model: Option<ModelScope>,
}

/// One entry of the endpoint's `limits[]` (kind/percent/scope) or of the CLI's
/// `model_scoped[]` (display_name/utilization). Both shapes map onto this struct.
#[derive(Debug, Deserialize)]
struct LimitEntry {
    kind:         Option<String>,
    percent:      Option<f64>,
    utilization:  Option<f64>,
    resets_at:    Option<serde_json::Value>,
    scope:        Option<LimitScope>,
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ExtraUsage {
    is_enabled:    Option<bool>,
    monthly_limit: Option<f64>,
    used_credits:  Option<f64>,
    utilization:   Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct UsageResponse {
    five_hour:    Option<UsageWindow>,
    seven_day:    Option<UsageWindow>,
    limits:       Option<Vec<LimitEntry>>,
    model_scoped: Option<Vec<LimitEntry>>,
    extra_usage:  Option<ExtraUsage>,
}

fn parse_reset(v: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    match v? {
        serde_json::Value::String(s) => DateTime::parse_from_rfc3339(s.trim())
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        serde_json::Value::Number(n) => {
            let x = n.as_f64()?;
            let secs = if x > 1.0e12 { x / 1000.0 } else { x };
            DateTime::from_timestamp(secs as i64, 0)
        }
        _ => None,
    }
}

fn pct_to_unit(v: Option<f64>) -> Option<f32> {
    v.map(|x| (x / 100.0).clamp(0.0, 1.0) as f32)
}

fn window_row(label: &str, w: Option<&UsageWindow>) -> UsageRow {
    UsageRow {
        label:       label.to_string(),
        utilization: pct_to_unit(w.and_then(|w| w.utilization)),
        reset:       parse_reset(w.and_then(|w| w.resets_at.as_ref())),
    }
}

fn build_usage(ur: &UsageResponse, tier: Option<String>) -> ClaudeUsage {
    let mut rows = vec![
        window_row("5h", ur.five_hour.as_ref()),
        window_row("7d", ur.seven_day.as_ref()),
    ];

    // Endpoint shape: limits[] with kind == "weekly_scoped" and scope.model.display_name.
    for l in ur.limits.iter().flatten() {
        if l.kind.as_deref() != Some("weekly_scoped") { continue; }
        let Some(name) = l.scope.as_ref()
            .and_then(|s| s.model.as_ref())
            .and_then(|m| m.display_name.clone())
        else { continue };
        rows.push(UsageRow {
            label:       name,
            utilization: pct_to_unit(l.percent.or(l.utilization)),
            reset:       parse_reset(l.resets_at.as_ref()),
        });
    }
    // CLI shape: rate_limits.model_scoped[] with display_name / utilization.
    for m in ur.model_scoped.iter().flatten() {
        let Some(name) = m.display_name.clone() else { continue };
        if rows.iter().any(|r| r.label == name) { continue; }
        rows.push(UsageRow {
            label:       name,
            utilization: pct_to_unit(m.utilization.or(m.percent)),
            reset:       parse_reset(m.resets_at.as_ref()),
        });
    }

    if let Some(x) = ur.extra_usage.as_ref() {
        let limit = x.monthly_limit.unwrap_or(0.0);
        if x.is_enabled == Some(true) && limit > 0.0 {
            let util = x.utilization
                .map(|u| u / 100.0)
                .or_else(|| x.used_credits.map(|c| c / limit))
                .map(|u| u.clamp(0.0, 1.0) as f32);
            rows.push(UsageRow { label: "Extra".to_string(), utilization: util, reset: None });
        }
    }

    ClaudeUsage { rows, fetched_at: Some(Utc::now()), tier }
}

// ─── Primary: dedicated OAuth usage endpoint ──────────────────────────────────

async fn fetch_usage_endpoint(client: &Client, token: &str) -> Result<UsageResponse, FetchError> {
    debug!("[claude] GET {}", USAGE_URL);
    let resp = client
        .get(USAGE_URL)
        .header("authorization", format!("Bearer {}", token))
        .header("anthropic-beta", OAUTH_BETA)
        .send()
        .await
        .map_err(|e| FetchError::Other(format!("HTTP error: {}", e)))?;

    let status = resp.status();
    info!("[claude] usage endpoint status: {}", status);

    match status.as_u16() {
        200..=299 => {
            let body = resp.text().await.map_err(|e| FetchError::Other(format!("read error: {}", e)))?;
            debug!("[claude] usage body: {}", &body[..body.len().min(1500)]);
            serde_json::from_str::<UsageResponse>(&body)
                .map_err(|e| FetchError::Other(format!("usage JSON parse error: {}", e)))
        }
        429 => Err(FetchError::RateLimited),
        401 => Err(FetchError::Auth("usage endpoint 401 Unauthorized".to_string())),
        _ => {
            let body = resp.text().await.unwrap_or_default();
            warn!("[claude] usage endpoint error body: {}", &body[..body.len().min(300)]);
            Err(FetchError::Other(format!("usage endpoint {}: {}", status, &body[..body.len().min(160)])))
        }
    }
}

// ─── Refresh path: headless Claude Code CLI `get_usage` control request ────────

#[derive(Debug, Deserialize)]
struct CliEnvelope {
    #[serde(rename = "type")]
    kind:     String,
    response: Option<CliControlResponse>,
}

#[derive(Debug, Deserialize)]
struct CliControlResponse {
    subtype:    String,
    request_id: Option<String>,
    error:      Option<String>,
    response:   Option<CliUsageBody>,
}

#[derive(Debug, Deserialize)]
struct CliUsageBody {
    rate_limits_available: Option<bool>,
    rate_limits:           Option<UsageResponse>,
    subscription_type:     Option<String>,
}

fn cli_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from("claude"),
        home_dir().join(".local").join("bin").join("claude.exe"),
        home_dir().join(".local").join("bin").join("claude"),
    ]
}

enum CliOutcome {
    /// CLI answered with plan windows.
    Usage(UsageResponse, Option<String>),
    /// CLI ran and refreshed credentials, but the usage endpoint was rate-limited.
    RateLimited,
    /// CLI ran but this login has no plan rate limits (API key / 3P provider).
    NoPlanLimits,
}

async fn cli_get_usage() -> Result<CliOutcome, String> {
    {
        let mut last = LAST_CLI_SPAWN.lock();
        if let Some(t) = *last {
            if t.elapsed() < CLI_MIN_GAP {
                return Err(format!("CLI refresh cooldown ({}s left)",
                    (CLI_MIN_GAP - t.elapsed()).as_secs()));
            }
        }
        *last = Some(Instant::now());
    }

    let request = format!(
        "{{\"type\":\"control_request\",\"request_id\":\"{}\",\"request\":{{\"subtype\":\"get_usage\"}}}}\n",
        CLI_REQUEST_ID
    );

    let mut last_err = String::from("claude CLI not found");
    for exe in cli_candidates() {
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("spawn {} failed: {}", exe.display(), e);
                debug!("[claude] {}", last_err);
                continue;
            }
        };
        info!("[claude] CLI get_usage via {}", exe.display());

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(request.as_bytes()).await {
                warn!("[claude] CLI stdin write failed: {}", e);
            }
            let _ = stdin.shutdown().await;
            drop(stdin);
        }

        let output = match tokio::time::timeout(CLI_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return Err(format!("CLI wait failed: {}", e)),
            Err(_) => return Err(format!("CLI timed out after {}s", CLI_TIMEOUT.as_secs())),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            debug!("[claude] CLI stderr: {}", stderr.trim().chars().take(600).collect::<String>());
        }

        for line in stdout.lines() {
            let line = line.trim();
            if !line.contains("control_response") { continue; }
            let Ok(env) = serde_json::from_str::<CliEnvelope>(line) else { continue };
            if env.kind != "control_response" { continue; }
            let Some(r) = env.response else { continue };
            if r.request_id.as_deref() != Some(CLI_REQUEST_ID) { continue; }
            if r.subtype != "success" {
                return Err(format!("CLI get_usage error: {}", r.error.unwrap_or_default()));
            }
            let Some(body) = r.response else { return Err("CLI get_usage: empty response".into()) };
            if body.rate_limits_available == Some(false) {
                return Ok(CliOutcome::NoPlanLimits);
            }
            return match body.rate_limits {
                Some(ur) => Ok(CliOutcome::Usage(ur, body.subscription_type)),
                None => Ok(CliOutcome::RateLimited),
            };
        }

        return Err(format!(
            "CLI exited with {} without a get_usage response ({})",
            output.status,
            stderr.trim().lines().last().unwrap_or("").chars().take(200).collect::<String>()
        ));
    }
    Err(last_err)
}

// ─── API-key fallback: POST /v1/messages, read rate-limit headers ─────────────

const PROBE_BODY: &str = r#"{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"."}]}"#;

fn parse_f32_hdr(headers: &HeaderMap, name: &str) -> Option<f32> {
    headers.get(name)?.to_str().ok()?.parse().ok()
}

fn parse_dt_hdr(headers: &HeaderMap, name: &str) -> Option<DateTime<Utc>> {
    let s = headers.get(name)?.to_str().ok()?.trim().to_string();
    if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(ts) = s.parse::<i64>() {
        return DateTime::from_timestamp(ts, 0);
    }
    warn!("[claude] could not parse reset header: '{}'", s);
    None
}

async fn fetch_via_messages(client: &Client, api_key: &str) -> Result<ClaudeUsage, FetchError> {
    info!("[claude] POST {} (api-key probe)", MESSAGES_URL);
    let resp = client
        .post(MESSAGES_URL)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .header("x-api-key", api_key)
        .body(PROBE_BODY)
        .send()
        .await
        .map_err(|e| FetchError::Other(format!("HTTP error: {}", e)))?;

    let status = resp.status();
    let headers = resp.headers().clone();
    info!("[claude] messages probe status: {}", status);

    if status.as_u16() == 401 {
        return Err(FetchError::Auth("Claude API key rejected (401)".to_string()));
    }
    if !status.is_success() && status.as_u16() != 429
        && !headers.contains_key("anthropic-ratelimit-unified-5h-utilization")
    {
        let body = resp.text().await.unwrap_or_default();
        return Err(FetchError::Other(format!("Claude API {}: {}", status, &body[..body.len().min(160)])));
    }

    let unit = |v: Option<f32>| v.map(|x| if x > 1.0 { x / 100.0 } else { x }.clamp(0.0, 1.0));
    let rows = vec![
        UsageRow {
            label: "5h".into(),
            utilization: unit(parse_f32_hdr(&headers, "anthropic-ratelimit-unified-5h-utilization")),
            reset: parse_dt_hdr(&headers, "anthropic-ratelimit-unified-5h-reset"),
        },
        UsageRow {
            label: "7d".into(),
            utilization: unit(parse_f32_hdr(&headers, "anthropic-ratelimit-unified-7d-utilization")),
            reset: parse_dt_hdr(&headers, "anthropic-ratelimit-unified-7d-reset"),
        },
    ];
    Ok(ClaudeUsage { rows, fetched_at: Some(Utc::now()), tier: None })
}

// ─── Public entry point ───────────────────────────────────────────────────────

pub async fn fetch(client: &Client) -> Result<ClaudeUsage, FetchError> {
    let loaded = load_token()?;

    if loaded.is_api_key {
        return fetch_via_messages(client, &loaded.token).await;
    }

    let tier = loaded.tier.clone();

    if !is_expiring(loaded.expires_at) {
        match fetch_usage_endpoint(client, &loaded.token).await {
            Ok(ur) => {
                let usage = build_usage(&ur, tier);
                info!("[claude] usage → {}", summarize(&usage));
                return Ok(usage);
            }
            Err(FetchError::Auth(reason)) => {
                warn!("[claude] {} — asking Claude Code CLI to refresh", reason);
            }
            Err(e) => return Err(e),
        }
    } else {
        info!("[claude] token expiring (expires_at={:?}) — asking Claude Code CLI to refresh",
              loaded.expires_at);
    }

    match cli_get_usage().await {
        Ok(CliOutcome::Usage(ur, sub)) => {
            let tier = load_token().ok().and_then(|t| t.tier)
                .or(tier)
                .or_else(|| tier_label(None, sub.as_deref()));
            let usage = build_usage(&ur, tier);
            info!("[claude] usage (via CLI) → {}", summarize(&usage));
            Ok(usage)
        }
        Ok(CliOutcome::RateLimited) => {
            info!("[claude] CLI refreshed credentials but usage endpoint is rate-limited");
            Err(FetchError::RateLimited)
        }
        Ok(CliOutcome::NoPlanLimits) => Err(FetchError::Other(
            "This Claude login has no plan rate limits (API key / 3P provider)".to_string())),
        Err(e) => {
            warn!("[claude] CLI refresh failed: {}", e);
            Err(FetchError::Auth(format!("token expired · open Claude Code once to refresh ({})", e)))
        }
    }
}

/// Diagnostic entry for `usage-monitor --probe [--cli]`: run one fetch and describe it.
/// `force_cli` exercises the Claude Code CLI refresh path even with a valid token.
pub async fn probe(client: &Client, force_cli: bool) -> String {
    if force_cli {
        return match cli_get_usage().await {
            Ok(CliOutcome::Usage(ur, sub)) => {
                let tier = load_token().ok().and_then(|t| t.tier).or_else(|| tier_label(None, sub.as_deref()));
                format!("CLI ok → {}", summarize(&build_usage(&ur, tier)))
            }
            Ok(CliOutcome::RateLimited) => "CLI ok, but usage endpoint rate-limited (rate_limits null)".to_string(),
            Ok(CliOutcome::NoPlanLimits) => "CLI ok, no plan rate limits for this login".to_string(),
            Err(e) => format!("CLI failed: {}", e),
        };
    }
    match fetch(client).await {
        Ok(u) => format!("ok → {} (tier={:?})", summarize(&u), u.tier),
        Err(e) => format!("error: {:?} ({})", e, e),
    }
}

fn summarize(u: &ClaudeUsage) -> String {
    u.rows
        .iter()
        .map(|r| format!("{}={}", r.label, r.utilization.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or("-".into())))
        .collect::<Vec<_>>()
        .join(" ")
}
