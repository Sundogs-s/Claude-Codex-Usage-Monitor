/// Cursor usage data fetcher.
///
/// Authentication: manual Cookie string stored at
///   %APPDATA%\UsageMonitor\cursor_cookie.txt
///
/// API endpoints (cursor.com — personal plan):
///   GET https://cursor.com/api/usage-summary
///     → included plan usage %, on-demand usage %, on-demand USD, billing reset date
///   GET https://cursor.com/api/auth/me
///     → user email / name (optional, best-effort)
use crate::state::CursorUsage;
use chrono::DateTime;
use log::{debug, info, warn};
use reqwest::Client;
use serde::Deserialize;
use std::path::PathBuf;

// ─── Cookie storage ──────────────────────────────────────────────────────────

pub fn cookie_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata)
        .join("UsageMonitor")
        .join("cursor_cookie.txt")
}

/// Load the stored Cookie header string (trimmed, single line).
pub fn load_cookie() -> Option<String> {
    let path = cookie_path();
    let raw = std::fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

/// Persist a Cookie header string to disk (overwrites previous value).
pub fn save_cookie(cookie: &str) -> Result<(), String> {
    let path = cookie_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir failed: {}", e))?;
    }
    std::fs::write(&path, cookie.trim())
        .map_err(|e| format!("write cookie failed: {}", e))
}

/// Delete the stored cookie (effectively signs out Cursor).
pub fn clear_cookie() {
    let _ = std::fs::remove_file(cookie_path());
}

// ─── API response structs ─────────────────────────────────────────────────────

/// Actual response shape from GET /api/usage-summary (confirmed from live traffic):
///
/// {
///   "billingCycleStart": "2026-04-14T07:10:36.000Z",
///   "billingCycleEnd":   "2026-05-14T07:10:36.000Z",
///   "membershipType": "pro",
///   "isUnlimited": false,
///   "individualUsage": {
///     "plan": {
///       "enabled": true,
///       "used":    2000,
///       "limit":   2000,
///       "remaining": 0,
///       "breakdown": { "included": 2000, "bonus": 170, "total": 2170 },
///       "autoPercentUsed": 2.13,   // % of AUTO (cheaper) model budget used
///       "apiPercentUsed":  41.0    // % of API (named model) budget used
///     },
///     "onDemand": { ... }          // may be absent on free/pro without on-demand
///   }
/// }
///
/// We parse via serde_json::Value for resilience against further schema changes.

/// Response from GET /api/auth/me — best effort.
#[derive(Debug, Deserialize, Default)]
struct AuthMe {
    email: Option<String>,
    name:  Option<String>,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn parse_rfc3339(s: &str) -> Option<DateTime<chrono::Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Parse the actual /api/usage-summary JSON into CursorUsage.
///
/// Uses Value traversal so minor schema additions don't break the parser.
fn parse_usage_summary(v: &serde_json::Value) -> CursorUsage {
    // ── Billing reset ─────────────────────────────────────────────────────────
    let reset_date = v["billingCycleEnd"]
        .as_str()
        .and_then(parse_rfc3339);

    // ── Plan usage: individualUsage.plan ─────────────────────────────────────
    let plan = &v["individualUsage"]["plan"];

    // autoPercentUsed: percentage of Auto (cheap) model budget consumed (0–100+).
    let auto_usage_pct: Option<f32> = plan["autoPercentUsed"]
        .as_f64()
        .map(|pct| (pct / 100.0).clamp(0.0, 1.0) as f32);

    // apiPercentUsed: percentage of named-model (API/premium) budget consumed (0–100+).
    let api_usage_pct: Option<f32> = plan["apiPercentUsed"]
        .as_f64()
        .map(|pct| (pct / 100.0).clamp(0.0, 1.0) as f32);

    info!(
        "[cursor] parsed → auto={:?}  api={:?}  reset={:?}",
        auto_usage_pct, api_usage_pct, reset_date
    );

    CursorUsage {
        auto_usage_pct,
        api_usage_pct,
        reset_date,
        user_email: None, // filled in by fetch()
    }
}

// ─── Fetchers ─────────────────────────────────────────────────────────────────

async fn fetch_usage_summary(client: &Client, cookie: &str) -> Result<serde_json::Value, String> {
    let url = "https://cursor.com/api/usage-summary";
    info!("[cursor] GET {}", url);

    let resp = client
        .get(url)
        .header("cookie", cookie)
        .header("accept", "application/json")
        .header("referer", "https://cursor.com/settings")
        .header(
            "user-agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/125.0 Safari/537.36",
        )
        .send()
        .await
        .map_err(|e| format!("network error: {}", e))?;

    let status = resp.status();
    info!("[cursor] usage-summary status: {}", status);

    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("Cookie 已过期或无效，请重新粘贴 Cookie".to_string());
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        warn!("[cursor] error body: {}", &body[..body.len().min(300)]);
        return Err(format!("HTTP {}: {}", status, &body[..body.len().min(200)]));
    }

    let body = resp.text().await.map_err(|e| format!("read body: {}", e))?;
    // Log full body for diagnostics (truncated at 2000 chars)
    debug!("[cursor] usage-summary body: {}", &body[..body.len().min(2000)]);

    serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|e| format!("JSON parse: {}", e))
}

async fn fetch_auth_me(client: &Client, cookie: &str) -> Option<AuthMe> {
    let url = "https://cursor.com/api/auth/me";
    info!("[cursor] GET {}", url);

    let resp = client
        .get(url)
        .header("cookie", cookie)
        .header("accept", "application/json")
        .header(
            "user-agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/125.0 Safari/537.36",
        )
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        warn!("[cursor] auth/me status: {}", resp.status());
        return None;
    }

    let body = resp.text().await.ok()?;
    debug!("[cursor] auth/me body: {}", &body[..body.len().min(200)]);
    serde_json::from_str::<AuthMe>(&body).ok()
}

// ─── Public entry point ───────────────────────────────────────────────────────

pub async fn fetch(client: &Client) -> Result<CursorUsage, String> {
    let cookie = load_cookie()
        .ok_or_else(|| "未配置 Cursor Cookie — 请右键菜单 → Cursor Cookie… 粘贴".to_string())?;

    // Fetch usage-summary (required) and auth/me (optional) in parallel.
    let (summary_res, me_opt) = tokio::join!(
        fetch_usage_summary(client, &cookie),
        fetch_auth_me(client, &cookie),
    );

    let summary = summary_res?;

    let mut usage = parse_usage_summary(&summary);
    usage.user_email = me_opt.and_then(|m| m.email.or(m.name));

    info!(
        "[cursor] → auto={:?}  api={:?}  reset={:?}",
        usage.auto_usage_pct,
        usage.api_usage_pct,
        usage.reset_date,
    );

    Ok(usage)
}
