//! Read-only telemetry: no threads, turns, or model requests.
use crate::state::{CodexUsage, CodexWindow, DailyTokens};
use chrono::{DateTime, Local, Utc};
use log::{info, warn};
use reqwest::Client;
use serde_json::{json, Value};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, process::Command};

pub struct DisplayRow {
    pub label: String,
    pub utilization: Option<f32>,
    pub reset: Option<DateTime<Utc>>,
    pub value: Option<String>,
}
fn metric(label: &str, value: String) -> DisplayRow {
    DisplayRow { label: label.into(), utilization: None, reset: None, value: Some(value) }
}
fn compact_number(n: u64) -> String {
    if n >= 1_000_000_000 { format!("{:.1}B", n as f64 / 1e9) }
    else if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) }
    else if n >= 1_000 { format!("{:.1}K", n as f64 / 1e3) }
    else { n.to_string() }
}
fn is_pro(plan: Option<&str>) -> bool {
    plan.map(|p| p.to_ascii_lowercase().starts_with("pro")).unwrap_or(false)
}

impl CodexUsage {
    pub fn plan_label(&self) -> String {
        match self.plan.as_deref() {
            Some("prolite") => "Pro Lite".into(), Some("pro") => "Pro".into(),
            Some("plus") => "Plus".into(), Some("business") | Some("team") => "Business".into(),
            Some(p) => p.into(), None => "".into(),
        }
    }
    pub fn visible_windows(&self) -> impl Iterator<Item = &CodexWindow> {
        self.windows.iter().filter(|w| !(is_pro(self.plan.as_deref()) && w.minutes == 300))
    }
    pub fn primary_utilization(&self) -> Option<f32> {
        self.visible_windows().next().map(|w| w.utilization)
    }
    // Three rows maximum: shared by the floating window and 40px taskbar.
    pub fn display_rows(&self) -> Vec<DisplayRow> {
        let mut rows: Vec<_> = self.visible_windows().take(2).map(|w| DisplayRow {
            label: w.label.clone(), utilization: Some(w.utilization), reset: w.reset, value: None,
        }).collect();
        let today = Local::now().format("%Y-%m-%d").to_string();
        let day = self.daily.as_ref().and_then(|days| days.iter().find(|d| d.date == today)
            .or_else(|| days.iter().filter(|d| d.date < today).max_by_key(|d| &d.date)));
        rows.push(match day {
            Some(d) if d.date == today => metric("DAY", format!("{} tok", compact_number(d.tokens))),
            Some(d) => metric("LAST", format!("{} tok {}", compact_number(d.tokens), &d.date[5..])),
            None => metric("DAY", "no data".into()),
        });
        if rows.len() < 3 {
            if let Some(credits) = &self.credits { rows.push(metric("CR", credits.clone())); }
        }
        if rows.len() < 3 {
            if let Some(n) = self.reset_count { rows.push(metric("RST", n.to_string())); }
        }
        if rows.len() == 1 {
            rows.push(metric("SYNC", if self.fetched_at.is_some() { "no quota data".into() } else { "waiting".into() }));
        }
        rows
    }
    pub fn tooltip(&self) -> String {
        let mut parts = vec![self.plan_label()];
        if let Some(w) = self.visible_windows().next() {
            parts.push(format!("{} {:.0}%", w.label, w.utilization * 100.0));
        }
        if self.visible_windows().next().is_none() {
            if let Some(row) = self.display_rows().first() {
                parts.push(format!("{} {}", row.label, row.value.as_deref().unwrap_or("no data")));
            }
        }
        if let Some(n) = self.reset_count { parts.push(format!("resets {}", n)); }
        parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }
    pub fn retain_activity(&mut self, previous: &Self) {
        if self.activity_fetched_at.is_none() && self.account_id.is_some() && self.account_id == previous.account_id {
            self.daily = previous.daily.clone();
            self.lifetime_tokens = previous.lifetime_tokens;
            self.activity_fetched_at = previous.activity_fetched_at;
        }
    }
    pub fn details(&self) -> String {
        let stamp = |t: Option<DateTime<Utc>>| t.map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "no data".into());
        let mut lines = vec![format!("Codex {}", self.plan_label()),
            format!("Quota updated: {}", stamp(self.fetched_at)), format!("Activity updated: {}", stamp(self.activity_fetched_at))];
        for w in self.visible_windows() {
            lines.push(format!("{}: {:.0}% used; reset {}", w.label, w.utilization * 100.0, stamp(w.reset)));
        }
        if self.limit_reached { lines.push("Usage limit reached".into()); }
        lines.push(format!("Credits: {}", self.credits.as_deref().unwrap_or("no data")));
        lines.push(format!("Available resets: {}", self.reset_count.map(|n| n.to_string()).unwrap_or_else(|| "no data".into())));
        lines.push("Daily token activity (latest 7 returned dates):".into());
        match &self.daily {
            Some(days) if !days.is_empty() => {
                for d in days.iter().rev().take(7).rev() { lines.push(format!("  {}  {} tokens", d.date, d.tokens)); }
            }
            _ => lines.push("  no data".into()),
        }
        if let Some(n) = self.lifetime_tokens { lines.push(format!("Lifetime: {} tokens", n)); }
        if let Some(error) = &self.activity_error { lines.push(format!("Activity: {}", error)); }
        lines.push("Dates follow the service's daily buckets; reporting may be delayed. Tokens are activity, not subscription quota. Queries do not run a model.".into());
        lines.join("\r\n")
    }
}

fn window_label(minutes: u64) -> String {
    if minutes % 1440 == 0 { format!("{}d", minutes / 1440) }
    else if minutes % 60 == 0 { format!("{}h", minutes / 60) }
    else { format!("{}m", minutes) }
}
fn add_window(out: &mut CodexUsage, window: &Value, camel: bool, prefix: &str) {
    let (pct_key, duration_key, reset_key) = if camel { ("usedPercent", "windowDurationMins", "resetsAt") }
        else { ("used_percent", "limit_window_seconds", "reset_at") };
    let Some(pct) = window[pct_key].as_f64().filter(|p| p.is_finite()) else { return; };
    let Some(duration) = window[duration_key].as_u64().filter(|n| *n > 0) else { return; };
    let minutes = if camel { duration } else { duration.div_ceil(60) };
    if is_pro(out.plan.as_deref()) && minutes == 300 { return; }
    let label = window_label(minutes);
    out.windows.push(CodexWindow {
        label: if prefix.is_empty() { label } else { format!("{} {}", prefix, label) },
        minutes, utilization: (pct / 100.0).clamp(0.0, 1.0) as f32,
        reset: window[reset_key].as_i64().and_then(|ts| DateTime::from_timestamp(ts, 0)),
    });
}
fn balance_text(v: &Value) -> Option<String> {
    if v["unlimited"].as_bool() == Some(true) { return Some("unlimited".into()); }
    let balance = v["balance"].as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v["balance"].as_f64())
        .filter(|n| n.is_finite() && *n >= 0.0)?;
    Some(format!("{:.2}", balance))
}
fn parse_limits(v: &Value, account: Option<&Value>) -> Result<CodexUsage, String> {
    let mut out = CodexUsage { fetched_at: Some(Utc::now()), ..Default::default() };
    let buckets = v["rateLimitsByLimitId"].as_object();
    let general = buckets.and_then(|m| m.get("codex")).unwrap_or(&v["rateLimits"]);
    out.plan = general["planType"].as_str().or_else(|| account.and_then(|a| a["account"]["planType"].as_str())).map(str::to_owned);
    out.account_id = v["accountId"].as_str().map(str::to_owned);
    out.credits = balance_text(&general["credits"]);
    out.reset_count = v["rateLimitResetCredits"]["availableCount"].as_u64();
    let mut read_bucket = |bucket: &Value, prefix: &str| {
        out.limit_reached |= bucket["rateLimitReachedType"].as_str().is_some() || bucket["spendControlReached"].as_bool() == Some(true);
        add_window(&mut out, &bucket["primary"], true, prefix);
        add_window(&mut out, &bucket["secondary"], true, prefix);
    };
    if let Some(buckets) = buckets.filter(|b| !b.is_empty()) {
        if let Some(b) = buckets.get("codex") { read_bucket(b, ""); }
        for (id, b) in buckets.iter().filter(|(id, _)| id.as_str() != "codex") {
            read_bucket(b, b["limitName"].as_str().unwrap_or(id));
        }
    } else if general.is_object() { read_bucket(general, ""); }
    else if v.get("rateLimits").map(Value::is_null) == Some(true) && out.plan.is_some() {
        // An explicit absent limit on an authenticated plan still allows activity rows.
    } else { return Err("Codex returned no quota snapshot".into()); }
    Ok(out)
}
fn parse_activity(out: &mut CodexUsage, v: &Value) {
    out.lifetime_tokens = v["summary"]["lifetimeTokens"].as_u64();
    out.daily = v["dailyUsageBuckets"].as_array().map(|days| {
        let mut days: Vec<_> = days.iter().filter_map(|d| {
            let date = d["startDate"].as_str()?;
            if date.len() != 10 || chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() { return None; }
            Some(DailyTokens { date: date.into(), tokens: d["tokens"].as_u64()? })
        }).collect();
        days.sort_by(|a, b| a.date.cmp(&b.date));
        days
    });
    out.activity_fetched_at = Some(Utc::now());
}

// Native binaries only: cancellation must not leave a shell's child running.
fn resolve_cli() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("USAGE_MONITOR_CODEX_CLI") {
        let p = PathBuf::from(p); return crate::platform::executable(&p).then_some(p);
    }
    #[cfg(not(windows))]
    {
        let candidates = crate::platform::executable_dirs().into_iter().map(|p| p.join("codex"))
            .chain([
                PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
                PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
                crate::platform::home_dir().join("Applications/Codex.app/Contents/Resources/codex"),
            ]);
        return candidates.into_iter().find(|p| crate::platform::executable(p));
    }
    #[cfg(windows)]
    {
    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    let newest = std::fs::read_dir(local.join("OpenAI/Codex/bin")).ok().into_iter().flatten().filter_map(Result::ok)
        .map(|d| d.path().join("codex.exe")).filter(|p| p.is_file())
        .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok());
    if newest.is_some() { return newest; }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).map(|d| d.join("codex.exe")).collect()).unwrap_or_default();
    let appdata = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default());
    let triple = if cfg!(target_arch = "aarch64") { "aarch64-pc-windows-msvc" } else { "x86_64-pc-windows-msvc" };
    let package = if cfg!(target_arch = "aarch64") { "codex-win32-arm64" } else { "codex-win32-x64" };
    let npm = appdata.join("npm/node_modules/@openai/codex");
    candidates.push(npm.join("node_modules/@openai").join(package).join("vendor").join(triple).join("codex/codex.exe"));
    candidates.push(npm.join("vendor").join(triple).join("codex/codex.exe"));
    if let Some(home) = std::env::var_os("USERPROFILE") { candidates.push(PathBuf::from(home).join(".local/bin/codex.exe")); }
    candidates.into_iter().find(|p| p.is_file())
    }
}
async fn send(input: &mut tokio::process::ChildStdin, v: Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(&v).map_err(|_| "Cannot encode Codex query".to_string())?;
    line.push(b'\n');
    input.write_all(&line).await.map_err(|_| "Cannot write Codex query".to_string())?;
    input.flush().await.map_err(|_| "Cannot flush Codex query".to_string())
}
async fn fetch_app_server(path: PathBuf) -> Result<CodexUsage, String> {
    let mut command = Command::new(&path);
    command.args(["app-server", "--stdio"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    if let Some(path) = crate::platform::child_path() { command.env("PATH", path); }
    let mut child = command.spawn().map_err(|_| "Cannot start Codex app-server".to_string())?;
    let mut input = child.stdin.take().ok_or("Codex stdin unavailable")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("Codex stdout unavailable")?).lines();
    let (mut limits, mut activity, mut account) = (None, None, None);
    let mut activity_error = Some("activity query timed out".to_string());
    let mut quota_error = "Codex quota query timed out".to_string();
    let transaction = async {
        send(&mut input, json!({"id":0,"method":"initialize","params":{
            "clientInfo":{"name":"usage_monitor","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}})).await?;
        let mut received = [false; 3];
        let mut initialized = false;
        while let Some(line) = lines.next_line().await.map_err(|_| "Cannot read Codex response".to_string())? {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue; };
            let Some(id) = v["id"].as_u64() else { continue; };
            if id == 0 && !initialized {
                if v.get("error").is_some() { return Err("Codex initialization failed".into()); }
                initialized = true;
                send(&mut input, json!({"method":"initialized"})).await?;
                // Strictly read-only. No thread/start, turn/start, or earned-reset consume.
                send(&mut input, json!({"id":1,"method":"account/rateLimits/read"})).await?;
                send(&mut input, json!({"id":2,"method":"account/usage/read"})).await?;
                send(&mut input, json!({"id":3,"method":"account/read","params":{"refreshToken":false}})).await?;
            } else if initialized && (1..=3).contains(&id) {
                received[(id - 1) as usize] = true;
                // Never log account/protocol bodies or credentials.
                let result = v.get("result").cloned();
                match id {
                    1 => { limits = result; if limits.is_none() { quota_error = "Codex quota query failed; check login".into(); } }
                    2 => { activity = result; activity_error = activity.is_none().then(|| "activity query unavailable; update Codex CLI or check login".into()); }
                    3 => account = result,
                    _ => {}
                }
                if received.iter().all(|x| *x) { break; }
            }
        }
        Ok::<(), String>(())
    };
    let completed = tokio::time::timeout(Duration::from_secs(25), transaction).await;
    drop(input);
    let _ = child.kill().await;
    let _ = child.wait().await;
    if let Ok(Err(e)) = completed { if limits.is_none() { return Err(e); } }
    let mut out = parse_limits(&limits.ok_or(quota_error)?, account.as_ref())?;
    if let Some(v) = activity { parse_activity(&mut out, &v); }
    out.activity_error = activity_error;
    info!("[codex] read-only app-server: plan={:?}, windows={}, activity={}", out.plan, out.windows.len(), out.activity_fetched_at.is_some());
    Ok(out)
}
fn decode_jwt(s: &str) -> Option<Value> {
    let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let segment = s.split('.').nth(1)?.replace('-', "+").replace('_', "/");
    let (mut output, mut buf, mut bits) = (Vec::new(), 0u32, 0u8);
    for c in segment.chars().take_while(|c| *c != '=') {
        buf = (buf << 6) | alphabet.find(c)? as u32;
        bits += 6;
        if bits >= 8 { bits -= 8; output.push((buf >> bits) as u8); }
    }
    serde_json::from_slice(&output).ok()
}
fn parse_wham(v: &Value, plan: Option<&str>) -> Result<CodexUsage, String> {
    let rate = &v["rate_limit"];
    if !rate.is_object() { return Err("Codex returned no quota snapshot".into()); }
    let mut out = CodexUsage {
        plan: v["plan_type"].as_str().or(plan).map(str::to_owned), fetched_at: Some(Utc::now()),
        credits: balance_text(&v["credits"]), limit_reached: rate["limit_reached"].as_bool().unwrap_or(false),
        activity_error: Some("daily activity needs a recent Codex CLI".into()), ..Default::default()
    };
    add_window(&mut out, &rate["primary_window"], false, "");
    add_window(&mut out, &rate["secondary_window"], false, "");
    Ok(out)
}
async fn fetch_wham(client: &Client) -> Result<CodexUsage, String> {
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(||
        crate::platform::home_dir().join(".codex"));
    let text = std::fs::read_to_string(home.join("auth.json")).map_err(|_| "Codex login unavailable; sign in to Codex".to_string())?;
    let auth: Value = serde_json::from_str(&text).map_err(|_| "Cannot parse Codex credentials".to_string())?;
    let token = auth["tokens"]["access_token"].as_str().filter(|s| !s.is_empty())
        .ok_or("Codex subscription data needs ChatGPT login (API keys are separate)")?;
    let claims = auth["tokens"]["id_token"].as_str().and_then(decode_jwt).unwrap_or(Value::Null);
    let claims = &claims["https://api.openai.com/auth"];
    let account = auth["tokens"]["account_id"].as_str().or_else(|| claims["chatgpt_account_id"].as_str());
    let mut request = client.get("https://chatgpt.com/backend-api/wham/usage").bearer_auth(token);
    if let Some(id) = account { request = request.header("ChatGPT-Account-Id", id); }
    let response = request.send().await.map_err(|_| "Codex usage network error".to_string())?;
    if !response.status().is_success() { return Err(format!("Codex usage HTTP {}; check login", response.status().as_u16())); }
    let v: Value = response.json().await.map_err(|_| "Cannot decode Codex usage".to_string())?;
    let mut out = parse_wham(&v, claims["chatgpt_plan_type"].as_str())?;
    out.account_id = account.map(str::to_owned);
    Ok(out)
}
pub async fn fetch(client: &Client) -> Result<CodexUsage, String> {
    if let Some(path) = resolve_cli() {
        match fetch_app_server(path).await {
            Ok(usage) => return Ok(usage),
            Err(e) => warn!("[codex] {}; trying read-only WHAM", e),
        }
    }
    fetch_wham(client).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plus_zero_5h_and_missing_week() {
        let out = parse_limits(&json!({"rateLimits":{"planType":"plus","primary":{
            "usedPercent":0,"windowDurationMins":300},"secondary":null}}), None).unwrap();
        assert_eq!(out.windows.len(), 1); assert_eq!(out.windows[0].label, "5h");
        assert_eq!(out.primary_utilization(), Some(0.0));
        assert!(out.display_rows().iter().all(|r| r.label != "7d"));
    }
    #[test]
    fn pro_variants_hide_5h_and_weekly_primary_is_7d() {
        for plan in ["pro", "prolite", "pro_5x", "pro_20x"] {
            let out = parse_limits(&json!({"rateLimits":{"planType":plan,
                "primary":{"usedPercent":10,"windowDurationMins":300},
                "secondary":{"usedPercent":6,"windowDurationMins":10080}}}), None).unwrap();
            assert_eq!(out.windows.len(), 1); assert_eq!(out.windows[0].label, "7d");
        }
        let out = parse_wham(&json!({"plan_type":"prolite","rate_limit":{
            "primary_window":{"used_percent":6,"limit_window_seconds":604800},"secondary_window":null}}), None).unwrap();
        assert_eq!(out.windows[0].label, "7d");
    }
    #[test]
    fn missing_zero_and_unlimited_are_distinct() {
        assert!(parse_limits(&json!({}), None).is_err());
        let out = parse_limits(&json!({"rateLimits":{"planType":"pro","primary":null,
            "credits":{"unlimited":false,"balance":"0"}}}), None).unwrap();
        assert_eq!(out.credits.as_deref(), Some("0.00")); assert_eq!(out.primary_utilization(), None);
        assert!(out.display_rows().iter().all(|r| r.utilization.is_none()));
        assert_eq!(balance_text(&json!({})), None);
        assert_eq!(balance_text(&json!({"unlimited":true})).as_deref(), Some("unlimited"));
    }
    #[test]
    fn multi_bucket_durations_and_account_plan_fallback() {
        let out = parse_limits(&json!({"rateLimitsByLimitId":{
            "codex":{"primary":{"usedPercent":6,"windowDurationMins":10080}},
            "spark":{"limitName":"Spark","primary":{"usedPercent":42,"windowDurationMins":60}},
            "other":{"primary":{"usedPercent":5,"windowDurationMins":300}}
        }}), Some(&json!({"account":{"planType":"prolite"}}))).unwrap();
        assert_eq!(out.windows.len(), 2); assert_eq!(out.windows[1].label, "Spark 1h");
        assert_eq!(out.plan.as_deref(), Some("prolite"));
    }
    #[test]
    fn delayed_activity_is_dated_and_null_is_unknown() {
        let mut out = CodexUsage::default();
        parse_activity(&mut out, &json!({"dailyUsageBuckets":[
            {"startDate":"2020-01-02","tokens":25562024},{"startDate":"bad","tokens":1}]}));
        let row = out.display_rows().remove(0);
        assert_eq!(row.label, "LAST"); assert_eq!(row.value.as_deref(), Some("25.6M tok 01-02"));
        parse_activity(&mut out, &json!({"dailyUsageBuckets":null}));
        assert_eq!(out.display_rows()[0].value.as_deref(), Some("no data"));
        let today = Local::now().format("%Y-%m-%d").to_string();
        parse_activity(&mut out, &json!({"dailyUsageBuckets":[{"startDate":today,"tokens":0}]}));
        assert_eq!(out.display_rows()[0].value.as_deref(), Some("0 tok"));
    }
    #[test]
    fn authenticated_pro_without_quota_still_shows_activity() {
        let mut out = parse_limits(&json!({"rateLimits":null,"rateLimitsByLimitId":null}),
            Some(&json!({"account":{"type":"chatgpt","planType":"pro"}}))).unwrap();
        let today = Local::now().format("%Y-%m-%d").to_string();
        parse_activity(&mut out, &json!({"dailyUsageBuckets":[{"startDate":today,"tokens":1234}]}));
        assert!(out.windows.is_empty());
        assert_eq!(out.display_rows()[0].value.as_deref(), Some("1.2K tok"));
        assert!(out.tooltip().contains("DAY 1.2K tok"));
    }
    #[test]
    fn stale_activity_is_retained_only_for_same_account() {
        let previous = CodexUsage { account_id: Some("a".into()), daily: Some(vec![DailyTokens {
            date: "2020-01-02".into(), tokens: 123 }]), activity_fetched_at: Some(Utc::now()), ..Default::default() };
        let mut next = CodexUsage { account_id: Some("b".into()), ..Default::default() };
        next.retain_activity(&previous); assert!(next.daily.is_none());
        next.account_id = Some("a".into()); next.retain_activity(&previous);
        assert_eq!(next.daily, previous.daily);
    }
}
