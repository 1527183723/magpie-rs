//! The built-in page: a small single-file UI served on loopback with a key.
//!
//! Upstream magpie ships a 5 MB Svelte app built by wails. magpie-rs is a
//! single HTML file with no build step, because the whole point of this port is
//! that `cargo build` produces a working APK with nothing else installed.

use crate::config::{Provider, Settings};
use crate::gateway::AppState;
use crate::log;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use serde_json::{Value, json};

const PAGE: &str = include_str!("web/index.html");

fn cookie_name(s: &Settings) -> String {
    format!("magpie_web_{}", s.web_port)
}

fn authorized(
    st: &AppState,
    headers: &HeaderMap,
    query: &std::collections::HashMap<String, String>,
) -> bool {
    let key = st
        .inner
        .cfg
        .read()
        .map(|c| c.settings.web_key.clone())
        .unwrap_or_default();
    if key.is_empty() {
        return true;
    }
    if query.get("k").map(|k| k == &key).unwrap_or(false) {
        return true;
    }
    let name = cookie_name(
        &st.inner
            .cfg
            .read()
            .map(|c| c.settings.clone())
            .unwrap_or_default(),
    );
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|c| {
            c.split(';')
                .filter_map(|kv| kv.trim().split_once('='))
                .any(|(k, v)| k == name && v == key)
        })
        .unwrap_or(false)
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/api/state", get(state_json))
        .route("/api/log", get(log_json))
        .route("/api/provider", post(upsert_provider))
        .route("/api/provider/delete", post(delete_provider))
        .route("/api/settings", post(save_settings))
        .route("/api/key", post(rotate_key))
        .route("/api/clear-log", post(clear_log))
        .route("/api/health", get(health))
        .with_state(state)
}

async fn page(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !authorized(&st, &headers, &q) {
        return (
            StatusCode::UNAUTHORIZED,
            Html(
                "<!doctype html><meta name=viewport content=\"width=device-width\">\
                 <body style=\"font:16px system-ui;padding:2rem\">\
                 <h3>magpie</h3><p>This page needs its key. Open it with \
                 <code>?k=…</code> as printed in the app's log.</p></body>",
            ),
        )
            .into_response();
    }
    let key = st
        .inner
        .cfg
        .read()
        .map(|c| c.settings.web_key.clone())
        .unwrap_or_default();
    let mut resp = Html(PAGE).into_response();
    if q.get("k").map(|k| k == &key).unwrap_or(false) {
        // Same lifetime the upstream page uses, so the WebView stays signed in
        // across app restarts.
        let cookie = format!(
            "{}={}; Path=/; Max-Age={}; SameSite=Lax",
            cookie_name(
                &st.inner
                    .cfg
                    .read()
                    .map(|c| c.settings.clone())
                    .unwrap_or_default()
            ),
            key,
            400 * 24 * 3600
        );
        if let Ok(v) = cookie.parse() {
            resp.headers_mut().insert(header::SET_COOKIE, v);
        }
    }
    resp
}

async fn health(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let q = std::collections::HashMap::new();
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (requests, errors) = st.inner.counters();
    Json(json!({"ok": true, "requests": requests, "errors": errors})).into_response()
}

async fn state_json(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let cfg = st.inner.cfg.read().unwrap_or_else(|e| e.into_inner());
    let (requests, errors) = st.inner.counters();
    let providers: Vec<Value> = cfg
        .providers
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.display(),
                "protocol": p.protocol.as_str(),
                "base_url": p.base_url,
                "models": p.models,
                "enabled": p.enabled,
                "priority": p.priority,
                "key_set": !p.api_key.is_empty(),
            })
        })
        .collect();
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": st.inner.uptime_secs(),
        "requests": requests,
        "errors": errors,
        "route": match cfg.settings.route {
            crate::config::RouteMode::Order => "order",
            crate::config::RouteMode::Rotate => "rotate",
            crate::config::RouteMode::Failover => "failover",
        },
        "require_key": cfg.settings.require_key,
        "log_level": cfg.settings.log_level,
        "web_port": cfg.settings.web_port,
        "gateway_addr": st.inner.opts().gateway_addr.clone(),
        "keys": cfg.settings.keys,
        "providers": providers,
        "usage": st.inner.ledger().totals(),
        "recent": st.inner.ledger().recent(30),
        "log_path": log::path().map(|p| p.display().to_string()),
    }))
    .into_response()
}

async fn log_json(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let n = q
        .get("n")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(300);
    Json(json!({"lines": log::tail(n.min(2000))})).into_response()
}

#[derive(Deserialize)]
struct ProviderForm {
    id: String,
    #[serde(default)]
    name: String,
    protocol: String,
    base_url: String,
    #[serde(default)]
    api_key: String,
    /// One model per line, as typed into the textarea.
    #[serde(default)]
    models: String,
    #[serde(default)]
    priority: i32,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

async fn upsert_provider(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<ProviderForm>,
) -> Response {
    let q = std::collections::HashMap::new();
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let protocol = match form.protocol.parse::<crate::config::Protocol>() {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    let models: Vec<String> = form
        .models
        .split(['\n', ','])
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();

    let mut cfg = st.inner.cfg.write().unwrap_or_else(|e| e.into_inner());
    let id = form.id.trim().to_string();
    if id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "id is required"})),
        )
            .into_response();
    }
    let provider = Provider {
        id: id.clone(),
        name: if form.name.trim().is_empty() {
            id.clone()
        } else {
            form.name.trim().to_string()
        },
        protocol,
        base_url: form.base_url.trim().trim_end_matches('/').to_string(),
        api_key: form.api_key.trim().to_string(),
        models,
        enabled: form.enabled,
        priority: form.priority,
        headers: Default::default(),
        extra: Default::default(),
    };
    match cfg.providers.iter_mut().find(|p| p.id == id) {
        Some(slot) => *slot = provider,
        None => cfg.providers.push(provider),
    }
    let saved = cfg.save();
    drop(cfg);
    crate::info!("provider `{id}` saved from the page");
    match saved {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct IdForm {
    id: String,
}

async fn delete_provider(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<IdForm>,
) -> Response {
    let q = std::collections::HashMap::new();
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut cfg = st.inner.cfg.write().unwrap_or_else(|e| e.into_inner());
    cfg.providers.retain(|p| p.id != form.id);
    let saved = cfg.save();
    drop(cfg);
    match saved {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct SettingsForm {
    #[serde(default)]
    route: Option<String>,
    #[serde(default)]
    require_key: Option<String>,
    #[serde(default)]
    allow_ips: Option<String>,
    #[serde(default)]
    log_level: Option<String>,
}

async fn save_settings(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<SettingsForm>,
) -> Response {
    let q = std::collections::HashMap::new();
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut cfg = st.inner.cfg.write().unwrap_or_else(|e| e.into_inner());
    if let Some(route) = form.route {
        cfg.settings.route = match route.as_str() {
            "rotate" => crate::config::RouteMode::Rotate,
            "failover" => crate::config::RouteMode::Failover,
            _ => crate::config::RouteMode::Order,
        };
    }
    if let Some(require) = form.require_key {
        cfg.settings.require_key = require == "on" || require == "1" || require == "true";
    }
    if let Some(ips) = form.allow_ips {
        cfg.settings.allow_ips = ips
            .split(['\n', ','])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Some(level) = form.log_level {
        cfg.settings.log_level = level;
    }
    let saved = cfg.save();
    drop(cfg);
    match saved {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct KeyForm {
    /// `gateway` adds an `sk-magpie-key-…`, `web` rotates the page key.
    #[serde(default)]
    what: String,
}

async fn rotate_key(
    State(st): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<KeyForm>,
) -> Response {
    let q = std::collections::HashMap::new();
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut cfg = st.inner.cfg.write().unwrap_or_else(|e| e.into_inner());
    let mut out = json!({"ok": true});
    match form.what.as_str() {
        "web" => {
            cfg.settings.web_key = crate::config::random_key(32);
            out["web_key"] = json!(cfg.settings.web_key);
        }
        _ => {
            let key = crate::config::new_gateway_key();
            cfg.settings.keys.push(key.clone());
            out["key"] = json!(key);
        }
    }
    let _ = cfg.save();
    Json(out).into_response()
}

async fn clear_log(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let q = std::collections::HashMap::new();
    if !authorized(&st, &headers, &q) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    log::clear();
    crate::info!("log cleared from the page");
    Json(json!({"ok": true})).into_response()
}
