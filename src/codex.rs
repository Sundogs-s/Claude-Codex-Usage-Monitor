/// Codex usage data fetcher.
///
/// The Codex CLI (ChatGPT OAuth mode) polls:
///   GET https://chatgpt.com/backend-api/wham/usage
///   Authorization: Bearer <tokens.access_token>
///   ChatGPT-Account-Id: <account_id from id_token JWT>
///
/// This is the same endpoint the Codex CLI itself polls every ~60 s.
/// Reference: openai/codex issue #10869, steipete/CodexBar docs/codex.md
use crate::state::WindowUsage;
use chrono::DateTime;
use log::{debug, info, warn};
use reqwest::Client;
use serde::Deserialize;
use std::path::PathBuf;

// ─── Credentials ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct CodexTokens {
    id_token:      Option<String>,
    access_token:  Option<String>,
    #[allow(dead_code)]
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct CodexAuth {
    auth_mode:      Option<String>,
    tokens:         Option<CodexTokens>,
    #[serde(rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    token:          Option<String>,
    access_token:   Option<String>,
    api_key:        Option<String>,
}

/// Resolved credentials ready to attach to a request.
struct Creds {
    /// Bearer token for Authorization header.
    access_token: String,
    /// Extracted from id_token JWT — needed for the WHAM endpoint.
    account_id:   Option<String>,
}

fn auth_path() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".codex").join("auth.json")
}

/// Decode the middle (payload) segment of a JWT without verifying the signature.
fn jwt_claim_account_id(jwt: &str) -> Option<String> {
    let payload_b64 = jwt.split('.').nth(1)?;
    // JWT uses base64url without padding — add padding.
    let padded = match payload_b64.len() % 4 {
        0 => payload_b64.to_string(),
        2 => format!("{}==", payload_b64),
        3 => format!("{}=", payload_b64),
        _ => payload_b64.to_string(),
    };
    let bytes = base64_decode(&padded)?;
    let payload: serde_json::Value = serde_json::from_slice(&bytes).ok()?;

    // The JWT includes an "https://api.openai.com/auth" claims block.
    payload
        .get("https://api.openai.com/auth")
        .and_then(|a| a.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Minimal base64url decoder (no external crate needed).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    // Replace URL-safe chars, then use a simple byte-by-byte decode.
    let s = s.replace('-', "+").replace('_', "/");
    // Standard base64 alphabet decode.
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u8;
    for c in s.chars() {
        if c == '=' { break; }
        let val = alphabet.find(c)? as u32;
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(output)
}

fn load_creds() -> Result<Creds, String> {
    let path = auth_path();
    info!("[codex] loading credentials from {}", path.display());
    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;

    let auth: CodexAuth = serde_json::from_str(&data)
        .map_err(|e| format!("Parse error in auth.json: {}", e))?;

    info!("[codex] auth_mode = {:?}", auth.auth_mode);

    // ── ChatGPT OAuth path ────────────────────────────────────────────────────
    if let Some(ref t) = auth.tokens {
        // access_token is the Bearer for WHAM; id_token carries account claims.
        let access = t.access_token.as_deref().filter(|s| !s.is_empty());
        let id_tok  = t.id_token.as_deref().filter(|s| !s.is_empty());

        if let Some(tok) = access {
            let account_id = id_tok.and_then(jwt_claim_account_id);
            info!(
                "[codex] OAuth creds: access_token {} chars, account_id={:?}",
                tok.len(),
                account_id
            );
            return Ok(Creds { access_token: tok.to_string(), account_id });
        }
        // Fallback: use id_token as bearer (some setups store only this).
        if let Some(tok) = id_tok {
            let account_id = jwt_claim_account_id(tok);
            info!("[codex] OAuth fallback: using id_token as bearer, account_id={:?}", account_id);
            return Ok(Creds { access_token: tok.to_string(), account_id });
        }
    }

    // ── API key path ─────────────────────────────────────────────────────────
    for (label, val) in [
        ("OPENAI_API_KEY", &auth.openai_api_key),
        ("token",          &auth.token),
        ("access_token",   &auth.access_token),
        ("api_key",        &auth.api_key),
    ] {
        if let Some(t) = val.as_deref().filter(|s| !s.is_empty()) {
            info!("[codex] using field '{}' as API key", label);
            return Ok(Creds { access_token: t.to_string(), account_id: None });
        }
    }

    Err("No usable token found in auth.json — check tokens.access_token".to_string())
}

// ─── Response parsing ─────────────────────────────────────────────────────────

/// Parse a string as DateTime — tries RFC3339 then Unix seconds.
fn parse_reset(s: &str) -> Option<DateTime<chrono::Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            s.parse::<i64>().ok().and_then(|ts| DateTime::from_timestamp(ts, 0))
        })
}

/// Parse the actual WHAM response shape (confirmed from live API):
///
/// {
///   "rate_limit": {
///     "primary_window":   { "used_percent": 35, "limit_window_seconds": 18000,  "reset_at": 1778231099 },
///     "secondary_window": { "used_percent": 47, "limit_window_seconds": 604800, "reset_at": 1778608773 }
///   }
/// }
///
/// primary_window   = 5h  (limit_window_seconds == 18000)
/// secondary_window = 7d  (limit_window_seconds == 604800)
/// used_percent is already 0–100; divide by 100 to get 0.0–1.0
fn parse_wham(v: &serde_json::Value) -> WindowUsage {
    if let Some(obj) = v.as_object() {
        let keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        debug!("[codex] top-level keys: {:?}", keys);
    }

    // ── Numeric path helper ───────────────────────────────────────────────────
    let num = |path: &[&str]| -> Option<f64> {
        let mut cur = v;
        for &key in path { cur = cur.get(key)?; }
        cur.as_f64()
    };

    // ── Shape A: confirmed live WHAM shape ────────────────────────────────────
    // rate_limit.primary_window   → 5h
    // rate_limit.secondary_window → 7d
    let pw = &v["rate_limit"]["primary_window"];
    let sw = &v["rate_limit"]["secondary_window"];

    let pct_5h   = pw.get("used_percent").and_then(|x| x.as_f64());
    let reset_5h_ts = pw.get("reset_at").and_then(|x| x.as_i64());
    let pct_7d   = sw.get("used_percent").and_then(|x| x.as_f64());
    let reset_7d_ts = sw.get("reset_at").and_then(|x| x.as_i64());

    if pct_5h.is_some() || pct_7d.is_some() {
        let u5h = pct_5h.map(|p| (p / 100.0).clamp(0.0, 1.0) as f32);
        let u7d = pct_7d.map(|p| (p / 100.0).clamp(0.0, 1.0) as f32);
        let r5h = reset_5h_ts.and_then(|ts| DateTime::from_timestamp(ts, 0));
        let r7d = reset_7d_ts.and_then(|ts| DateTime::from_timestamp(ts, 0));
        info!(
            "[codex] WHAM shape A: 5h={}% reset={:?} | 7d={}% reset={:?}",
            pct_5h.unwrap_or(0.0), r5h, pct_7d.unwrap_or(0.0), r7d
        );
        return WindowUsage {
            utilization_5h: u5h,
            utilization_7d: u7d,
            reset_5h: r5h,
            reset_7d: r7d,
        };
    }

    // ── Shape B: generic fallback (used_percent at various nesting levels) ────
    let pct_from_path = |path: &[&str]| -> Option<f32> {
        num(path).map(|p| (p / 100.0).clamp(0.0, 1.0) as f32)
    };
    let ts_from_path = |path: &[&str]| -> Option<DateTime<chrono::Utc>> {
        num(path).and_then(|ts| DateTime::from_timestamp(ts as i64, 0))
    };

    // used/limit ratio style (older or alternative shapes)
    let util_ratio = |used: Option<f64>, limit: Option<f64>| -> Option<f32> {
        match (used, limit) {
            (Some(u), Some(l)) if l > 0.0 => Some((u / l).clamp(0.0, 1.0) as f32),
            _ => None,
        }
    };

    let u5h = pct_from_path(&["five_hour", "used_percent"])
        .or_else(|| util_ratio(num(&["five_hour", "used"]), num(&["five_hour", "limit"])));
    let u7d = pct_from_path(&["seven_day", "used_percent"])
        .or_else(|| util_ratio(num(&["seven_day", "used"]), num(&["seven_day", "limit"])));
    let r5h = ts_from_path(&["five_hour", "reset_at"]);
    let r7d = ts_from_path(&["seven_day", "reset_at"]);

    debug!(
        "[codex] fallback shape: 5h={:?} | 7d={:?}",
        u5h, u7d
    );

    WindowUsage {
        utilization_5h: u5h,
        utilization_7d: u7d,
        reset_5h: r5h,
        reset_7d: r7d,
    }
}

// ─── API call ─────────────────────────────────────────────────────────────────

pub async fn fetch(client: &Client) -> Result<WindowUsage, String> {
    let creds = load_creds()?;

    // Primary: the WHAM endpoint the Codex CLI itself uses.
    let wham_url = "https://chatgpt.com/backend-api/wham/usage";

    // Fallback endpoints for API-key users.
    let fallback_urls: &[&str] = &[
        "https://api.openai.com/v1/orgs/me/monthly_budgets",
        "https://api.openai.com/dashboard/billing/usage",
    ];

    let all_urls = std::iter::once(wham_url)
        .chain(fallback_urls.iter().copied());

    for url in all_urls {
        info!("[codex] trying GET {}", url);

        let mut req = client
            .get(url)
            .bearer_auth(&creds.access_token)
            .header("content-type", "application/json");

        // The WHAM endpoint requires this header when using ChatGPT auth.
        if url.contains("chatgpt.com") {
            if let Some(ref id) = creds.account_id {
                req = req.header("ChatGPT-Account-Id", id.as_str());
                info!("[codex] adding ChatGPT-Account-Id: {}", id);
            }
        }

        let resp = match req.send().await {
            Ok(r)  => r,
            Err(e) => { warn!("[codex] network error on {}: {}", url, e); continue; }
        };

        let status = resp.status();
        let body   = resp.text().await.unwrap_or_default();
        info!("[codex] {} → {} ({} bytes)", url, status, body.len());
        // Always log the full body (up to 1 KB) so we can see the real shape.
        info!("[codex] body: {}", &body[..body.len().min(1000)]);

        if !status.is_success() {
            warn!("[codex] skipping non-2xx response from {}", url);
            continue;
        }

        let v: serde_json::Value = match serde_json::from_str(&body) {
            Ok(v)  => v,
            Err(e) => { warn!("[codex] JSON parse error: {}", e); continue; }
        };

        let usage = parse_wham(&v);
        info!(
            "[codex] parsed → 5h={:?}  7d={:?}  reset_5h={:?}",
            usage.utilization_5h, usage.utilization_7d, usage.reset_5h
        );

        // Return even if all fields are None — the caller will display what it has.
        return Ok(usage);
    }

    Err("All Codex endpoints failed — see log for per-URL status codes".to_string())
}
