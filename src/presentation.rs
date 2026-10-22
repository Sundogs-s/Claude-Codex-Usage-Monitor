//! Explicit, credential-free view model for native clients.
use crate::state::*;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub schema: u32,
    pub version: &'static str,
    pub services: Vec<ServiceView>,
    pub refresh_seconds: u64,
    pub cookie_configured: bool,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ServiceView {
    pub id: &'static str,
    pub title: &'static str,
    pub enabled: bool,
    pub meta: String,
    pub stale: bool,
    pub error: Option<&'static str>,
    pub updated: Option<i64>,
    pub rows: Vec<RowView>,
    pub activity: Vec<ActivityMetric>,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ActivityMetric {
    pub label: String,
    pub value: Option<String>,
}

/// Keep missing days distinct from a returned zero. Dates are provider buckets.
pub fn activity_metrics(usage: &CodexUsage, today: chrono::NaiveDate) -> Vec<ActivityMetric> {
    let yesterday = today.pred_opt().unwrap_or(today);
    let start = today - chrono::Duration::days(6);
    let lookup = |date: chrono::NaiveDate| usage.daily.as_ref()
        .and_then(|days| days.iter().find(|d| d.date == date.to_string()))
        .map(|d| format_tokens(d.tokens));
    let mut dates = std::collections::BTreeMap::new();
    for day in usage.daily.iter().flatten() {
        if let Ok(date) = chrono::NaiveDate::parse_from_str(&day.date, "%Y-%m-%d") {
            if date >= start && date <= today { dates.insert(date, day.tokens); }
        }
    }
    vec![
        ActivityMetric { label: "今日".into(), value: lookup(today) },
        ActivityMetric { label: "昨日".into(), value: lookup(yesterday) },
        ActivityMetric { label: if dates.len() == 7 { "近 7 天" } else { "近 7 天 · 已返回日期" }.into(),
            value: (!dates.is_empty()).then(|| format_tokens(dates.values().fold(0u64, |a,b| a.saturating_add(*b)))) },
    ]
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 { format!("{:.2}B", n as f64 / 1e9) }
    else if n >= 1_000_000 { format!("{:.2}M", n as f64 / 1e6) }
    else if n >= 1_000 { format!("{:.1}K", n as f64 / 1e3) }
    else { n.to_string() }
}

/// The full dashboard has no taskbar row limit. Never synthesize missing quotas.
pub fn dashboard_codex_rows(usage: &CodexUsage) -> Vec<RowView> {
    let mut rows: Vec<_> = usage.visible_windows().map(|w| RowView {
        label: w.label.clone(), utilization: Some(w.utilization),
        reset: w.reset.map(|t| t.timestamp()), value: None,
    }).collect();
    if rows.is_empty() { rows.push(RowView { label: "额度".into(), utilization: None, reset: None, value: None }); }
    if let Some(credits) = &usage.credits { rows.push(RowView { label: "Credits".into(), utilization: None, reset: None, value: Some(credits.clone()) }); }
    if let Some(n) = usage.reset_count { rows.push(RowView { label: "可重置次数".into(), utilization: None, reset: None, value: Some(n.to_string()) }); }
    rows
}

#[derive(Debug, Serialize)]
pub struct RowView {
    pub label: String,
    pub utilization: Option<f32>,
    pub reset: Option<i64>,
    pub value: Option<String>,
}

fn error(raw: &str) -> Option<&'static str> {
    if raw.is_empty() { None }
    else if raw.contains("Cookie") { Some("Cursor Cookie is missing or expired") }
    else if raw.contains("expired") || raw.contains("credentials") || raw.contains("Keychain") || raw.contains("login") {
        Some("Login unavailable or expired; open the provider CLI to sign in")
    } else if raw.contains("rate-limit") || raw.contains("429") { Some("Rate limited; retrying later") }
    else { Some("Cannot refresh; showing last known data when available") }
}

pub fn snapshot(state: &AppState) -> Snapshot {
    let settings = &state.settings;
    let mut codex_detail = state.codex.clone();
    codex_detail.activity_error = codex_detail.activity_error.map(|_| "Activity query unavailable; retry later".into());
    Snapshot {
        schema: 1, version: env!("CARGO_PKG_VERSION"),
        refresh_seconds: settings.refresh_rate.secs(),
        cookie_configured: crate::cursor::load_cookie().is_some(),
        message: None,
        services: vec![
            ServiceView {
                id: "claude", title: "Claude", enabled: settings.show_claude,
                meta: state.claude.tier.clone().unwrap_or_default(),
                stale: state.claude_stale || !state.claude_error.is_empty(),
                error: error(&state.claude_error), updated: state.claude.fetched_at.map(|t| t.timestamp()),
                rows: state.claude.rows.iter().map(|r| RowView { label: r.label.clone(),
                    utilization: r.utilization, reset: r.reset.map(|t| t.timestamp()), value: None }).collect(),
                activity: Vec::new(),
                detail: state.claude_next_retry.map(|t| format!("Next retry: {}", t.with_timezone(&chrono::Local).format("%H:%M:%S"))),
            },
            ServiceView {
                id: "codex", title: "Codex", enabled: settings.show_codex,
                meta: [state.codex.plan_label(), if state.codex.activity_error.is_some() { "partial".into() } else { String::new() },
                    if state.codex.limit_reached { "limited".into() } else { String::new() }]
                    .into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
                stale: !state.codex_error.is_empty(), error: error(&state.codex_error),
                updated: state.codex.fetched_at.map(|t| t.timestamp()),
                rows: dashboard_codex_rows(&state.codex),
                activity: activity_metrics(&state.codex, chrono::Local::now().date_naive()),
                detail: Some(codex_detail.details()),
            },
            ServiceView {
                id: "cursor", title: "Cursor", enabled: settings.show_cursor,
                meta: String::new(), stale: !state.cursor_error.is_empty(), error: error(&state.cursor_error),
                updated: None,
                rows: [("AUTO", state.cursor.auto_usage_pct), ("API", state.cursor.api_usage_pct)]
                    .into_iter().map(|(label, utilization)| RowView { label: label.into(), utilization,
                        reset: state.cursor.reset_date.map(|t| t.timestamp()), value: None }).collect(),
                activity: Vec::new(), detail: None,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_keeps_all_quota_windows_and_missing_activity() {
        let today=chrono::NaiveDate::from_ymd_opt(2026,10,13).unwrap();
        let mut usage=CodexUsage::default();
        assert!(activity_metrics(&usage,today).iter().all(|m|m.value.is_none()));
        usage.daily=Some(vec![DailyTokens{date:"2026-10-13".into(),tokens:0},
            DailyTokens{date:"2026-10-10".into(),tokens:1500},DailyTokens{date:"2026-01-01".into(),tokens:999999},
            DailyTokens{date:"2026-10-14".into(),tokens:999999}]);
        let metrics=activity_metrics(&usage,today);
        assert_eq!(metrics[0].value.as_deref(),Some("0"));
        assert!(metrics[1].value.is_none());
        assert_eq!(metrics[2].value.as_deref(),Some("1.5K"));
        assert!(metrics[2].label.contains("已返回"));
        usage.plan=Some("plus".into());
        usage.windows=(1..=6).map(|i|CodexWindow{label:format!("{}h",i),minutes:i*60,utilization:0.1,reset:None}).collect();
        assert_eq!(dashboard_codex_rows(&usage).len(),6);
    }
    #[test]
    fn native_view_keeps_missing_and_zero_distinct_and_omits_identity() {
        let mut state = AppState::default();
        state.cursor.user_email = Some("private@example.test".into());
        state.codex.account_id = Some("private-account-id".into());
        state.codex.plan = Some("pro".into());
        state.codex.windows = vec![
            CodexWindow { label: "5h".into(), minutes: 300, utilization: 0.8, reset: None },
            CodexWindow { label: "7d".into(), minutes: 10080, utilization: 0.0, reset: None },
        ];
        state.codex_error = "secret-server-response".into();
        let view = snapshot(&state);
        let codex = &view.services[1];
        assert_eq!(codex.rows[0].label, "7d");
        assert_eq!(codex.rows[0].utilization, Some(0.0));
        assert!(codex.stale);
        assert!(view.services[2].rows[0].utilization.is_none());
        let json = serde_json::to_string(&view).unwrap();
        for secret in ["private@example.test", "private-account-id", "secret-server-response"] {
            assert!(!json.contains(secret));
        }
    }
}
