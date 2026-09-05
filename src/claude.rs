/// Claude usage data fetcher (v0.5.3).
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
///   4. Expired / rejected token → silently launch the Claude Code CLI (no console
///      window) and ask it through its headless stream-json control protocol
///      (`get_usage`). The CLI refreshes the OAuth token with its own credential
///      locking, writes the new token back to the credentials file and answers with
///      the same windows (rate_limits.model_scoped). This monitor never touches the
///      refresh token, so it can never invalidate the Claude Code login.
///      The launch is bounded: at most `CLI_MAX_ATTEMPTS` per credentials-file
///      generation (mtime + expiresAt), spaced by `CLI_RETRY_DELAYS`. The counter
///      resets when the credentials file changes, on the tray "立即刷新" action, or
///      on restart. A missing CLI stops the attempts immediately.
///   5. API-key credentials (no plan limits) → POST /v1/messages probe headers (5h/7d only).
use crate::state::{ClaudeUsage, UsageRow};
use chrono::{DateTime, Utc};
use log::{debug, info, warn};
use parking_lot::Mutex;
use reqwest::{header::HeaderMap, Client};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncWriteExt;

const OAUTH_BETA: &str = "oauth-2025-04-20";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
/// Treat the token as expiring this long before `expiresAt` so a request never races expiry.
const EXPIRY_MARGIN_MS: i64 = 5 * 60 * 1000;
/// Hard timeout for one headless CLI round-trip (cold start of the CLI takes a few seconds).
const CLI_TIMEOUT: Duration = Duration::from_secs(90);
/// Maximum CLI launches per credentials-file generation.
const CLI_MAX_ATTEMPTS: u32 = 3;
/// Wait before attempt N+1 after attempt N failed (index = attempts already made − 1).
const CLI_RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(300), Duration::from_secs(900)];
const CLI_REQUEST_ID: &str = "usage-monitor";

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum FetchError {
    /// The usage endpoint answered 429. Keep last-known data and back off.
    RateLimited,
    /// Credentials are missing or unparsable: there is nothing to show.
    Auth(String),
    /// The OAuth token is expired and the CLI refresh has not fixed it (yet).
    /// Last-known rows stay visible as stale. `next_attempt` is the time of the next
    /// CLI launch, or None when attempts are exhausted / the CLI is missing.
    Expired { reason: String, next_attempt: Option<DateTime<Utc>> },
    Other(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::RateLimited => write!(f, "usage API rate-limited"),
            FetchError::Expired { reason, .. } => write!(f, "{}", reason),
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
    /// Identity of the credentials-file contents: (mtime, expiresAt). Changes whenever
    /// Claude Code rewrites the file, i.e. after every successful token refresh.
    generation: Generation,
}

type Generation = (Option<SystemTime>, Option<i64>);

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
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

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
                generation: (mtime, oauth.expires_at),
            });
        }
    }
    if let Some(key) = creds.api_key {
        if !key.is_empty() {
            info!("[claude] using api_key field");
            return Ok(LoadedToken { token: key, expires_at: None, is_api_key: true, tier: None,
                                    generation: (mtime, None) });
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

/// A resolved Claude Code CLI launcher.
#[derive(Debug, Clone)]
struct CliExe {
    path:   PathBuf,
    /// npm installs ship only a `claude.cmd` shim; CreateProcess cannot run it directly,
    /// so it goes through `cmd /C`.
    is_cmd: bool,
}

/// Locate the Claude Code CLI. Order: real executables (PATH, npm global
/// node_modules, ~/.local/bin, ~/.claude/local), then `.cmd` shims on PATH.
fn resolve_cli() -> Option<CliExe> {
    let home = home_dir();
    let appdata = PathBuf::from(std::env::var("APPDATA").unwrap_or_default());
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();

    let mut exes: Vec<PathBuf> = path_dirs.iter().map(|d| d.join("claude.exe")).collect();
    exes.push(appdata.join("npm").join("node_modules").join("@anthropic-ai")
        .join("claude-code").join("bin").join("claude.exe"));
    exes.push(home.join(".local").join("bin").join("claude.exe"));
    exes.push(home.join(".claude").join("local").join("claude.exe"));
    if let Some(p) = exes.into_iter().find(|p| p.is_file()) {
        return Some(CliExe { path: p, is_cmd: false });
    }

    let mut cmds: Vec<PathBuf> = path_dirs.iter().map(|d| d.join("claude.cmd")).collect();
    cmds.push(appdata.join("npm").join("claude.cmd"));
    cmds.into_iter().find(|p| p.is_file()).map(|p| CliExe { path: p, is_cmd: true })
}

// ─── Bounded refresh attempts ────────────────────────────────────────────────

#[derive(Debug, Default)]
struct RefreshTracker {
    generation:   Option<Generation>,
    attempts:     u32,
    next_allowed: Option<Instant>,
    /// No CLI on this machine: nothing to retry until the user acts (manual refresh / restart).
    cli_missing:  bool,
}

static TRACKER: Mutex<RefreshTracker> = Mutex::new(RefreshTracker {
    generation: None, attempts: 0, next_allowed: None, cli_missing: false,
});

/// Forget all refresh attempts (tray "立即刷新"): the next poll may launch the CLI again.
pub fn reset_refresh_attempts() {
    let mut t = TRACKER.lock();
    if t.attempts > 0 || t.cli_missing {
        info!("[claude] CLI refresh attempts reset (manual)");
    }
    *t = RefreshTracker::default();
}

fn instant_to_utc(at: Instant) -> DateTime<Utc> {
    let now = Instant::now();
    if at <= now { Utc::now() } else { Utc::now() + chrono::Duration::from_std(at - now).unwrap_or_default() }
}

/// Decide whether a CLI launch is allowed right now for this credentials generation.
/// Ok(attempt number) → go; Err(FetchError::Expired) → not now (reason + next time).
fn tracker_begin_attempt(generation: &Generation) -> Result<u32, FetchError> {
    let mut t = TRACKER.lock();
    if t.generation.as_ref() != Some(generation) {
        if t.generation.is_some() {
            info!("[claude] credentials file changed — CLI refresh attempts reset");
        }
        *t = RefreshTracker { generation: Some(generation.clone()), ..Default::default() };
    }
    if t.cli_missing {
        return Err(FetchError::Expired {
            reason: "token expired · Claude CLI not found · open Claude Code once to refresh".into(),
            next_attempt: None,
        });
    }
    if t.attempts >= CLI_MAX_ATTEMPTS {
        return Err(FetchError::Expired {
            reason: format!("token expired · CLI refresh gave up after {} attempts · open Claude Code once",
                            CLI_MAX_ATTEMPTS),
            next_attempt: None,
        });
    }
    if let Some(next) = t.next_allowed {
        if next > Instant::now() {
            return Err(FetchError::Expired {
                reason: format!("token expired · CLI refresh retry in {}s", (next - Instant::now()).as_secs()),
                next_attempt: Some(instant_to_utc(next)),
            });
        }
    }
    t.attempts += 1;
    let n = t.attempts;
    t.next_allowed = CLI_RETRY_DELAYS.get((n - 1) as usize).map(|d| Instant::now() + *d);
    Ok(n)
}

/// Record the outcome of a CLI launch; returns the `Expired` error to surface on failure.
fn tracker_after_failure(cli_missing: bool, detail: &str) -> FetchError {
    let mut t = TRACKER.lock();
    if cli_missing {
        t.cli_missing = true;
        return FetchError::Expired {
            reason: format!("token expired · Claude CLI not found ({}) · open Claude Code once to refresh", detail),
            next_attempt: None,
        };
    }
    if t.attempts >= CLI_MAX_ATTEMPTS {
        FetchError::Expired {
            reason: format!("token expired · CLI refresh failed {} times ({}) · open Claude Code once",
                            t.attempts, detail),
            next_attempt: None,
        }
    } else {
        FetchError::Expired {
            reason: format!("token expired · CLI refresh failed ({}) · retrying later", detail),
            next_attempt: t.next_allowed.map(instant_to_utc),
        }
    }
}

enum CliOutcome {
    /// CLI answered with plan windows.
    Usage(UsageResponse, Option<String>),
    /// CLI ran and refreshed credentials, but the usage endpoint was rate-limited.
    RateLimited,
    /// CLI ran but this login has no plan rate limits (API key / 3P provider).
    NoPlanLimits,
}

enum CliError {
    NotFound(String),
    Failed(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::NotFound(s) | CliError::Failed(s) => write!(f, "{}", s),
        }
    }
}

/// Launch the CLI once (no console window) and ask for `get_usage`.
async fn cli_get_usage() -> Result<CliOutcome, CliError> {
    let Some(exe) = resolve_cli() else {
        warn!("[claude] Claude CLI not found on PATH / npm global / ~/.local/bin / ~/.claude/local");
        return Err(CliError::NotFound("claude CLI not found".into()));
    };

    let request = format!(
        "{{\"type\":\"control_request\",\"request_id\":\"{}\",\"request\":{{\"subtype\":\"get_usage\"}}}}\n",
        CLI_REQUEST_ID
    );
    // --strict-mcp-config without --mcp-config: start no MCP servers at all.
    // (--bare is NOT usable here: it disables OAuth, so the token could not be refreshed.)
    let cli_args = ["-p", "--input-format", "stream-json", "--output-format", "stream-json",
                    "--verbose", "--strict-mcp-config"];

    let mut cmd = if exe.is_cmd {
        let mut c = tokio::process::Command::new("cmd.exe");
        c.arg("/C").arg(&exe.path).args(cli_args);
        c
    } else {
        let mut c = tokio::process::Command::new(&exe.path);
        c.args(cli_args);
        c
    };
    cmd.current_dir(std::env::temp_dir())
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
            warn!("[claude] spawn {} failed: {}", exe.path.display(), e);
            return Err(CliError::NotFound(format!("spawn {} failed: {}", exe.path.display(), e)));
        }
    };
    info!("[claude] CLI get_usage via {}{}", exe.path.display(), if exe.is_cmd { " (cmd /C)" } else { "" });

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(request.as_bytes()).await {
            warn!("[claude] CLI stdin write failed: {}", e);
        }
        let _ = stdin.shutdown().await;
        drop(stdin);
    }

    let output = match tokio::time::timeout(CLI_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Err(CliError::Failed(format!("CLI wait failed: {}", e))),
        Err(_) => return Err(CliError::Failed(format!("CLI timed out after {}s", CLI_TIMEOUT.as_secs()))),
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
            return Err(CliError::Failed(format!("CLI get_usage error: {}", r.error.unwrap_or_default())));
        }
        let Some(body) = r.response else { return Err(CliError::Failed("CLI get_usage: empty response".into())) };
        if body.rate_limits_available == Some(false) {
            return Ok(CliOutcome::NoPlanLimits);
        }
        return match body.rate_limits {
            Some(ur) => Ok(CliOutcome::Usage(ur, body.subscription_type)),
            None => Ok(CliOutcome::RateLimited),
        };
    }

    Err(CliError::Failed(format!(
        "CLI exited with {} without a get_usage response ({})",
        output.status,
        stderr.trim().lines().last().unwrap_or("").chars().take(200).collect::<String>()
    )))
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

    refresh_via_cli(&loaded, tier).await
}

/// Bounded CLI refresh: consult the tracker, launch at most once per call, and map the
/// outcome. A success that did not rewrite the credentials file still counts as an
/// attempt so a CLI that answers but never persists the token cannot be spawned forever.
async fn refresh_via_cli(loaded: &LoadedToken, tier: Option<String>) -> Result<ClaudeUsage, FetchError> {
    let attempt = tracker_begin_attempt(&loaded.generation)?;
    info!("[claude] CLI refresh attempt {}/{}", attempt, CLI_MAX_ATTEMPTS);

    match cli_get_usage().await {
        Ok(CliOutcome::Usage(ur, sub)) => {
            let reloaded = load_token().ok();
            match reloaded.as_ref().map(|t| &t.generation) {
                Some(g) if *g != loaded.generation => {
                    info!("[claude] CLI refreshed the credentials file (new expires_at={:?})",
                          reloaded.as_ref().and_then(|t| t.expires_at));
                    *TRACKER.lock() = RefreshTracker::default();
                }
                _ => warn!("[claude] CLI answered but the credentials file did not change \
                            (attempt {}/{}) — token will still read as expired next poll",
                           attempt, CLI_MAX_ATTEMPTS),
            }
            let tier = reloaded.and_then(|t| t.tier)
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
        Err(CliError::NotFound(e)) => {
            warn!("[claude] CLI refresh impossible: {}", e);
            Err(tracker_after_failure(true, &e))
        }
        Err(CliError::Failed(e)) => {
            warn!("[claude] CLI refresh failed: {}", e);
            Err(tracker_after_failure(false, &e))
        }
    }
}

/// Diagnostic entry for `usage-monitor --probe [--cli]`: run one fetch and describe it.
/// `force_cli` exercises the Claude Code CLI refresh path even with a valid token and
/// reports whether the CLI rewrote the credentials file.
pub async fn probe(client: &Client, force_cli: bool) -> String {
    if force_cli {
        let before = load_token().ok().map(|t| t.generation);
        let out = match cli_get_usage().await {
            Ok(CliOutcome::Usage(ur, sub)) => {
                let tier = load_token().ok().and_then(|t| t.tier).or_else(|| tier_label(None, sub.as_deref()));
                format!("CLI ok → {}", summarize(&build_usage(&ur, tier)))
            }
            Ok(CliOutcome::RateLimited) => "CLI ok, but usage endpoint rate-limited (rate_limits null)".to_string(),
            Ok(CliOutcome::NoPlanLimits) => "CLI ok, no plan rate limits for this login".to_string(),
            Err(e) => format!("CLI failed: {}", e),
        };
        let after = load_token().ok().map(|t| t.generation);
        let cli = resolve_cli().map(|c| format!("{}{}", c.path.display(), if c.is_cmd { " (cmd /C)" } else { "" }))
            .unwrap_or_else(|| "<not found>".into());
        return format!("{} | cli={} | credentials file {}", out, cli,
                       if before != after { "REWRITTEN (token refreshed)" } else { "unchanged" });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One test only: the tracker is a process-wide static.
    #[test]
    fn cli_refresh_attempts_are_bounded_and_resettable() {
        reset_refresh_attempts();
        let gen_a: Generation = (None, Some(1));
        let gen_b: Generation = (None, Some(2));

        // Attempt 1 goes immediately; attempt 2 must wait for the first delay.
        assert_eq!(tracker_begin_attempt(&gen_a).ok(), Some(1));
        match tracker_begin_attempt(&gen_a) {
            Err(FetchError::Expired { next_attempt: Some(_), .. }) => {}
            other => panic!("expected a scheduled retry, got {:?}", other.map(|_| ())),
        }

        // Exhaust the remaining attempts by clearing the delay each time.
        for n in 2..=CLI_MAX_ATTEMPTS {
            TRACKER.lock().next_allowed = None;
            assert_eq!(tracker_begin_attempt(&gen_a).ok(), Some(n));
        }
        TRACKER.lock().next_allowed = None;
        match tracker_begin_attempt(&gen_a) {
            Err(FetchError::Expired { next_attempt: None, reason }) => assert!(reason.contains("gave up")),
            other => panic!("expected exhaustion, got {:?}", other.map(|_| ())),
        }

        // A rewritten credentials file (new generation) starts over.
        assert_eq!(tracker_begin_attempt(&gen_b).ok(), Some(1));

        // A missing CLI stops attempts until a manual reset.
        let _ = tracker_after_failure(true, "not found");
        match tracker_begin_attempt(&gen_b) {
            Err(FetchError::Expired { next_attempt: None, reason }) => assert!(reason.contains("CLI not found")),
            other => panic!("expected cli-missing stop, got {:?}", other.map(|_| ())),
        }
        reset_refresh_attempts();
        assert_eq!(tracker_begin_attempt(&gen_b).ok(), Some(1));
        reset_refresh_attempts();
    }
}
