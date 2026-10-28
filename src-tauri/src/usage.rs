//! Anonymous usage counts, sent to Aptabase only when the user turns on
//! "Share anonymous usage counts". Each event says what happened (a tab was
//! closed and under which rule, the whole-browser limit paused, the mode
//! changed, the app started) plus the app and macOS versions. No process
//! names, websites, memory figures or identifiers are sent; the session ID is
//! random and new each launch. PRIVACY.md lists every event.
//!
//! Builds without an `APTABASE_APP_KEY` at compile time (local and
//! development builds) never send anything and don't offer the setting.

use crate::guard::Usage;
use serde_json::{json, Value};
use std::sync::OnceLock;

const APP_KEY: Option<&str> = option_env!("APTABASE_APP_KEY");

/// Whether this build can send usage counts at all.
pub fn available() -> bool {
    APP_KEY.is_some_and(|k| ingest_url(k).is_some())
}

/// Aptabase app keys name their region: "A-US-…" or "A-EU-…".
fn ingest_url(key: &str) -> Option<&'static str> {
    match key.split('-').nth(1)? {
        "US" => Some("https://us.aptabase.com/api/v0/events"),
        "EU" => Some("https://eu.aptabase.com/api/v0/events"),
        _ => None,
    }
}

/// "2026-10-28T09:13:05Z"
fn rfc3339(unix: u64) -> String {
    let t = unix as libc::time_t;
    let mut tm = unsafe { std::mem::zeroed::<libc::tm>() };
    unsafe { libc::gmtime_r(&t, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn mode_name(mode: crate::config::Mode) -> String {
    serde_json::to_string(&mode).unwrap_or_default().trim_matches('"').to_string()
}

/// The event name and properties for one recorded `Usage`.
pub fn describe(u: Usage) -> (&'static str, Value) {
    match u {
        Usage::Closed(rule) => ("tab_closed", json!({ "rule": rule })),
        Usage::Paused(why) => ("whole_browser_paused", json!({ "why": why })),
        Usage::ModeChanged(mode) => ("mode_changed", json!({ "mode": mode_name(mode) })),
    }
}

pub fn started(mode: crate::config::Mode) -> (&'static str, Value) {
    ("app_started", json!({ "mode": mode_name(mode) }))
}

/// One event in the shape Aptabase's ingest API takes.
pub fn event(name: &str, props: Value, session: &str, unix: u64, os_version: &str) -> Value {
    json!({
        "timestamp": rfc3339(unix),
        "sessionId": session,
        "eventName": name,
        "systemProps": {
            "isDebug": cfg!(debug_assertions),
            "osName": "macOS",
            "osVersion": os_version,
            "locale": "",
            "appVersion": env!("CARGO_PKG_VERSION"),
            "sdkVersion": concat!("tab-guard@", env!("CARGO_PKG_VERSION")),
        },
        "props": props,
    })
}

fn session_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| format!("{}{:010}", crate::guard::unix_now(), unsafe { libc::arc4random() }))
}

fn os_version() -> &'static str {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
}

/// Send events in the background. Failures (offline, say) are dropped:
/// nothing is queued or retried.
pub fn send(events: Vec<(&'static str, Value)>) {
    let (Some(key), false) = (APP_KEY, events.is_empty()) else { return };
    let Some(url) = ingest_url(key) else { return };
    let now = crate::guard::unix_now();
    let body: Vec<Value> =
        events.into_iter().map(|(name, props)| event(name, props, session_id(), now, os_version())).collect();
    tauri::async_runtime::spawn(async move {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build() else {
            return;
        };
        let _ = client.post(url).header("App-Key", key).json(&body).send().await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;

    #[test]
    fn region_comes_from_the_key() {
        assert_eq!(ingest_url("A-US-1234567890"), Some("https://us.aptabase.com/api/v0/events"));
        assert_eq!(ingest_url("A-EU-1234567890"), Some("https://eu.aptabase.com/api/v0/events"));
        assert_eq!(ingest_url("A-SH-1234567890"), None, "self-hosted isn't set up");
        assert_eq!(ingest_url("nonsense"), None);
    }

    #[test]
    fn timestamps_are_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_793_178_785), "2026-10-28T09:13:05Z");
    }

    #[test]
    fn events_carry_nothing_identifying() {
        let (name, props) = describe(Usage::Closed("whole-browser"));
        let e = event(name, props, "s1", 0, "26.2");
        assert_eq!(e["eventName"], "tab_closed");
        assert_eq!(e["props"], json!({ "rule": "whole-browser" }));
        assert_eq!(e["systemProps"]["osVersion"], "26.2");
        assert_eq!(e["systemProps"]["appVersion"], env!("CARGO_PKG_VERSION"));
        // Only these keys, so nothing else can slip in.
        let mut keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["eventName", "props", "sessionId", "systemProps", "timestamp"]);
    }

    #[test]
    fn every_usage_has_a_name() {
        assert_eq!(describe(Usage::Paused("hourly-cap")).0, "whole_browser_paused");
        assert_eq!(describe(Usage::ModeChanged(Mode::WarnThenClose)).1, json!({ "mode": "warn-then-close" }));
        assert_eq!(started(Mode::Watch), ("app_started", json!({ "mode": "watch" })));
    }

    #[test]
    fn local_builds_send_nothing() {
        // Tests are built without an app key.
        assert!(!available());
        send(vec![started(Mode::Watch)]);
    }
}
