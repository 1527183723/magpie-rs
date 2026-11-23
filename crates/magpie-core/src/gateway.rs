//! The HTTP gateway: OpenAI and Anthropic endpoints in, any provider out.

use crate::config::{Config, Protocol, SharedConfig};
use crate::ir::*;
use crate::log;
use crate::protocol::SseParser;
use crate::route::{self, Target};
use crate::usage::{Entry, Ledger};
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

pub struct GatewayOptions {
    /// Where `settings.json`, `providers.json` and `usage.jsonl` live.
    pub config_dir: PathBuf,
    /// `127.0.0.1:3425`, or `0.0.0.0:3425` when LAN access is on.
    pub gateway_addr: String,
    /// `Some("127.0.0.1:3430")` serves the built-in page; `None` disables it.
    pub web_addr: Option<String>,
    /// Refuse to start the page server without a key.
    pub web_key: Option<String>,
    pub log_path: Option<PathBuf>,
    pub echo_log: bool,
    /// Print a one-line summary per request instead of the full body.
    pub quiet: bool,
}

impl Default for GatewayOptions {
    fn default() -> Self {
        Self {
            config_dir: std::env::temp_dir().join("magpie-rs"),
            gateway_addr: "127.0.0.1:3425".into(),
            web_addr: Some("127.0.0.1:3430".into()),
            web_key: None,
            log_path: None,
            echo_log: true,
            quiet: false,
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) inner: Arc<Gateway>,
}

pub struct Gateway {
    pub cfg: SharedConfig,
    client: reqwest::Client,
    ledger: Arc<Ledger>,
    started: Instant,
    requests: AtomicU64,
    errors: AtomicU64,
    opts: GatewayOptions,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Gateway {
    pub fn new(opts: GatewayOptions) -> std::io::Result<Arc<Gateway>> {
        // reqwest is built with `rustls-no-provider` so the crypto backend is a
        // build-time choice rather than a CMake dependency; installing it here
        // is idempotent and harmless when something else already did.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut cfg = Config::load(&opts.config_dir)?;
        if let Some(key) = &opts.web_key
            && !key.is_empty()
        {
            cfg.settings.web_key = key.clone();
        }
        let _ = cfg.save();

        log::init(
            opts.log_path.as_deref(),
            opts.echo_log,
            cfg.settings.level(),
        );
        crate::info!(
            "magpie-rs {} starting; {} provider(s) configured",
            env!("CARGO_PKG_VERSION"),
            cfg.providers.len()
        );

        let client = reqwest::Client::builder()
            .user_agent(concat!("magpie-rs/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            // No overall timeout: a streaming completion legitimately runs for
            // minutes. Per-read stalls are caught by the connect timeout plus
            // the provider's own idle behaviour.
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .build()
            .map_err(|e| std::io::Error::other(format!("http client: {e}")))?;

        let ledger = Arc::new(Ledger::new(&opts.config_dir)?);
        Ok(Arc::new(Gateway {
            cfg: Arc::new(std::sync::RwLock::new(cfg)),
            client,
            ledger,
            started: Instant::now(),
            requests: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            opts,
        }))
    }

    pub fn opts(&self) -> &GatewayOptions {
        &self.opts
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    pub fn counters(&self) -> (u64, u64) {
        (
            self.requests.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
        )
    }

    fn settings(&self) -> crate::config::Settings {
        self.with_config(|c| c.settings.clone())
    }

    /// Read the config without ever letting a `RwLockReadGuard` live across an
    /// `await`. A guard held over a suspension point makes the whole handler
    /// `!Send`, which axum rejects at the type level with an error that points
    /// nowhere near the real cause.
    pub fn with_config<R>(&self, f: impl FnOnce(&Config) -> R) -> R {
        let guard = self.cfg.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// Serve the gateway, and the page when `web_addr` is set, until `shutdown`
    /// resolves. Both listeners are cancelled through one watch channel so a
    /// Ctrl-C on the desktop and a service stop on Android take the same path.
    pub async fn run(
        self: Arc<Self>,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let state = AppState {
            inner: self.clone(),
        };

        let gw_listener = tokio::net::TcpListener::bind(&self.opts.gateway_addr)
            .await
            .map_err(|e| {
                std::io::Error::other(format!(
                    "cannot bind gateway on {}: {e}",
                    self.opts.gateway_addr
                ))
            })?;
        crate::info!("gateway listening on http://{}/v1", self.opts.gateway_addr);

        let web_listener = match &self.opts.web_addr {
            Some(addr) => {
                let l = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
                    std::io::Error::other(format!("cannot bind the page on {addr}: {e}"))
                })?;
                let key = self.settings().web_key;
                crate::info!(
                    "page listening on http://{addr}/?k={}",
                    &key[..key.len().min(8)]
                );
                Some(l)
            }
            None => None,
        };

        let (tx, rx) = tokio::sync::watch::channel(());
        let mut rx_gw = rx.clone();
        let gw_app = api_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
        let gw_task = tokio::spawn(async move {
            let _ = axum::serve(gw_listener, gw_app)
                .with_graceful_shutdown(async move {
                    let _ = rx_gw.changed().await;
                })
                .await;
        });

        let web_task = web_listener.map(|l| {
            let mut rx_web = rx.clone();
            let app = crate::web::router(state);
            tokio::spawn(async move {
                let _ = axum::serve(l, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx_web.changed().await;
                    })
                    .await;
            })
        });

        shutdown.await;
        crate::info!("stopping");
        let _ = tx.send(());
        let _ = gw_task.await;
        if let Some(t) = web_task {
            let _ = t.await;
        }
        Ok(())
    }

    async fn call_upstream(
        &self,
        target: &Target,
        body: Value,
        stream: bool,
    ) -> Result<reqwest::Response, String> {
        let url = upstream_url(target, stream);
        let mut req = self.client.post(&url).json(&body);
        req = match target.protocol {
            Protocol::Openai => req.bearer_auth(&target.api_key),
            Protocol::Anthropic => req
                .header("x-api-key", &target.api_key)
                .header("anthropic-version", "2023-06-01"),
            Protocol::Gemini => req.header("x-goog-api-key", &target.api_key),
        };
        for (k, v) in &target.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if stream {
            req = req.header("accept", "text/event-stream");
        }
        crate::debug!("-> POST {url}");
        req.send().await.map_err(|e| format!("{url}: {e}"))
    }
}

fn upstream_url(target: &Target, stream: bool) -> String {
    let base = target.base_url.trim_end_matches('/');
    let has_version = base
        .rsplit('/')
        .next()
        .map(|seg| {
            seg.starts_with('v') && seg[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .unwrap_or(false);
    let root = if has_version {
        base.to_string()
    } else {
        match target.protocol {
            Protocol::Gemini => format!("{base}/v1beta"),
            _ => format!("{base}/v1"),
        }
    };
    match target.protocol {
        Protocol::Openai => format!("{root}/chat/completions"),
        Protocol::Anthropic => format!("{root}/messages"),
        Protocol::Gemini => {
            if stream {
                format!(
                    "{root}/models/{}:streamGenerateContent?alt=sse",
                    target.upstream_model
                )
            } else {
                format!("{root}/models/{}:generateContent", target.upstream_model)
            }
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(header::AUTHORIZATION)
        && let Ok(s) = v.to_str()
    {
        return Some(s.to_string());
    }
    for name in ["x-api-key", "x-goog-api-key", "api-key"] {
        if let Some(v) = headers.get(name)
            && let Ok(s) = v.to_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

fn api_error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    let body = json!({"error": {"type": kind, "message": message.into()}});
    (status, Json(body)).into_response()
}

/// Anthropic callers expect `{"type":"error","error":{"type":...}}`.
fn anthropic_error(status: StatusCode, kind: &str, message: impl Into<String>) -> Response {
    let body = json!({"type": "error", "error": {"type": kind, "message": message.into()}});
    (status, Json(body)).into_response()
}

fn api_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/magpie/info", get(info))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(json!({"ok": true, "service": "magpie-rs", "version": env!("CARGO_PKG_VERSION")}))
}

async fn info(State(st): State<AppState>) -> impl IntoResponse {
    let (requests, errors) = st.inner.counters();
    let cfg = st.inner.cfg.read().unwrap_or_else(|e| e.into_inner());
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": st.inner.uptime_secs(),
        "requests": requests,
        "errors": errors,
        "providers": cfg.enabled_providers().count(),
        "route": cfg.settings.route,
        "log": log::path().map(|p| p.display().to_string()),
    }))
}

async fn models(State(st): State<AppState>, headers: HeaderMap) -> Response {
    let cfg = st.inner.cfg.read().unwrap_or_else(|e| e.into_inner());
    if !cfg.key_ok(bearer(&headers).as_deref()) {
        return api_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "invalid gateway key",
        );
    }
    Json(json!({"object": "list", "data": route::catalogue(&cfg)})).into_response()
}

#[axum::debug_handler]
async fn chat_completions(
    State(st): State<AppState>,
    ConnectInfo(ci): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch(st, ci, headers, body, Dialect::Openai).await
}

#[axum::debug_handler]
async fn messages(
    State(st): State<AppState>,
    ConnectInfo(ci): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    dispatch(st, ci, headers, body, Dialect::Anthropic).await
}

async fn count_tokens(
    State(st): State<AppState>,
    ConnectInfo(ci): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ok = st.inner.with_config(|c| {
        c.key_ok(bearer(&headers).as_deref()) && c.ip_allowed(&ci.ip().to_string())
    });
    if !ok {
        return anthropic_error(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid gateway key",
        );
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return anthropic_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "body is not JSON",
        );
    };
    let Ok(req) = crate::protocol::anthropic::parse_request(&v) else {
        return anthropic_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "cannot parse request",
        );
    };
    let usage = crate::usage::estimate_request(&req);
    Json(json!({"input_tokens": usage.input_tokens})).into_response()
}

async fn dispatch(
    st: AppState,
    peer: SocketAddr,
    headers: HeaderMap,
    body: Bytes,
    dialect: Dialect,
) -> Response {
    let gw = st.inner.clone();
    gw.requests.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();

    let err = |status: StatusCode, kind: &str, msg: String| -> Response {
        gw.errors.fetch_add(1, Ordering::Relaxed);
        match dialect {
            Dialect::Anthropic => anthropic_error(status, kind, msg),
            _ => api_error(status, kind, msg),
        }
    };

    let (key_ok, ip_ok) = gw.with_config(|c| {
        (
            c.key_ok(bearer(&headers).as_deref()),
            c.ip_allowed(&peer.ip().to_string()),
        )
    });
    if !key_ok {
        return err(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid gateway key".into(),
        );
    }
    if !ip_ok {
        return err(
            StatusCode::FORBIDDEN,
            "permission_error",
            "source address is not allowed".into(),
        );
    }

    let Ok(value) = serde_json::from_slice::<Value>(&body) else {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "body is not JSON".into(),
        );
    };

    let parsed = match dialect {
        Dialect::Anthropic => crate::protocol::anthropic::parse_request(&value),
        _ => crate::protocol::openai::parse_request(&value),
    };
    let mut request = match parsed {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, "invalid_request_error", e),
    };

    let requested_model = request.model.clone();
    let targets = gw.with_config(|c| route::resolve(c, &requested_model));
    let targets = match targets {
        Ok(t) => t,
        Err(e) => return err(StatusCode::NOT_FOUND, "model_not_found", e.to_string()),
    };

    let stream_wanted = request.stream;
    let mut last_error = String::from("no provider answered");
    let mut last_status = StatusCode::BAD_GATEWAY;

    for target in &targets {
        let upstream_body = match target.protocol {
            Protocol::Anthropic => crate::protocol::anthropic::render_request(
                &request,
                &target.upstream_model,
                stream_wanted,
            ),
            Protocol::Gemini => crate::protocol::gemini::render_request(&request),
            Protocol::Openai => crate::protocol::openai::render_request(
                &request,
                &target.upstream_model,
                stream_wanted,
            ),
        };
        // The exact model string goes on the wire; the IR keeps the caller's.
        request.model = target.upstream_model.clone();

        let resp = match gw.call_upstream(target, upstream_body, stream_wanted).await {
            Ok(r) => r,
            Err(e) => {
                crate::warn!("{}: {e}", target.provider_id);
                last_error = e;
                last_status = StatusCode::BAD_GATEWAY;
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() {
            let ms = started.elapsed().as_millis() as u64;
            if stream_wanted {
                return stream_response(
                    gw.clone(),
                    resp,
                    target.protocol,
                    dialect,
                    &requested_model,
                    target,
                    started,
                );
            }
            let text = match resp.text().await {
                Ok(t) => t,
                Err(e) => {
                    last_error = format!("reading reply: {e}");
                    last_status = StatusCode::BAD_GATEWAY;
                    continue;
                }
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => {
                    crate::warn!(
                        "{}: reply is not JSON: {}",
                        target.provider_id,
                        truncate(&text, 200)
                    );
                    last_error = "upstream reply is not JSON".into();
                    last_status = StatusCode::BAD_GATEWAY;
                    continue;
                }
            };

            let (rendered, usage) = match target.protocol {
                Protocol::Anthropic => {
                    let parsed = crate::protocol::anthropic::parse_response(&value);
                    match parsed {
                        Ok(r) => {
                            let usage = r.usage.clone();
                            (
                                crate::protocol::anthropic::render_response(&r, &requested_model),
                                usage,
                            )
                        }
                        Err(e) => {
                            last_error = e;
                            last_status = StatusCode::BAD_GATEWAY;
                            continue;
                        }
                    }
                }
                Protocol::Gemini => {
                    let parsed = crate::protocol::gemini::parse_response(&value);
                    match parsed {
                        Ok(r) => {
                            let usage = r.usage.clone();
                            let resp = crate::ir::ChatResponse {
                                model: target.upstream_model.clone(),
                                ..r
                            };
                            let rendered = match dialect {
                                Dialect::Anthropic => crate::protocol::anthropic::render_response(
                                    &resp,
                                    &requested_model,
                                ),
                                _ => crate::protocol::openai::render_response(
                                    &resp,
                                    &requested_model,
                                ),
                            };
                            (rendered, usage)
                        }
                        Err(e) => {
                            last_error = e;
                            last_status = StatusCode::BAD_GATEWAY;
                            continue;
                        }
                    }
                }
                Protocol::Openai => {
                    let parsed = crate::protocol::openai::parse_response(&value);
                    match parsed {
                        Ok(r) => {
                            let usage = r.usage.clone();
                            let resp = crate::ir::ChatResponse {
                                model: target.upstream_model.clone(),
                                ..r
                            };
                            let rendered = match dialect {
                                Dialect::Anthropic => crate::protocol::anthropic::render_response(
                                    &resp,
                                    &requested_model,
                                ),
                                _ => crate::protocol::openai::render_response(
                                    &resp,
                                    &requested_model,
                                ),
                            };
                            (rendered, usage)
                        }
                        Err(e) => {
                            last_error = e;
                            last_status = StatusCode::BAD_GATEWAY;
                            continue;
                        }
                    }
                }
            };

            gw.ledger.append(&Entry {
                at: now_secs(),
                provider: target.provider_id.clone(),
                model: target.upstream_model.clone(),
                requested: requested_model.clone(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                ms,
                status: status.as_u16(),
                stream: false,
                dialect: match dialect {
                    Dialect::Anthropic => "anthropic".into(),
                    _ => "openai".into(),
                },
            });
            crate::info!(
                "{} -> {} ok  {} in / {} out  {} ms",
                requested_model,
                target.provider_id,
                usage.input_tokens,
                usage.output_tokens,
                ms
            );
            return (StatusCode::OK, Json(rendered)).into_response();
        }

        let body_text = resp.text().await.unwrap_or_default();
        crate::warn!(
            "{} answered {}: {}",
            target.provider_id,
            status.as_u16(),
            truncate(&body_text, 300)
        );
        last_error = truncate(&body_text, 500);
        last_status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        // A 4xx that is not a rate limit means "wrong request", not "wrong
        // provider": stop and surface it instead of burning the other keys.
        if status.as_u16() != 429 && status.is_client_error() {
            break;
        }
    }

    err(last_status, "upstream_error", last_error)
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

fn stream_response(
    gw: Arc<Gateway>,
    upstream: reqwest::Response,
    from: Protocol,
    to: Dialect,
    requested_model: &str,
    target: &Target,
    started: Instant,
) -> Response {
    let passthrough = matches!(
        (from, to),
        (Protocol::Openai, Dialect::Openai) | (Protocol::Anthropic, Dialect::Anthropic)
    );

    let body = if passthrough {
        let s = upstream
            .bytes_stream()
            .map(|r| r.map_err(|e| std::io::Error::other(e.to_string())));
        Body::from_stream(s)
    } else {
        let decode = match from {
            Protocol::Openai => Dialect::Openai,
            Protocol::Anthropic => Dialect::Anthropic,
            Protocol::Gemini => Dialect::Gemini,
        };
        Body::from_stream(convert_stream(
            upstream.bytes_stream(),
            decode,
            to,
            requested_model.to_string(),
        ))
    };

    // Record the request as soon as it is accepted; a stream's real token
    // counts arrive later and are logged from the converter.
    let provider = target.provider_id.clone();
    let model = target.upstream_model.clone();
    let requested = requested_model.to_string();
    let gw2 = gw.clone();
    let ledger_entry = Entry {
        at: now_secs(),
        provider,
        model,
        requested,
        input_tokens: 0,
        output_tokens: 0,
        ms: started.elapsed().as_millis() as u64,
        status: 200,
        stream: true,
        dialect: match to {
            Dialect::Anthropic => "anthropic".into(),
            _ => "openai".into(),
        },
    };
    gw2.ledger.append(&ledger_entry);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache, no-transform")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .body(body)
        .unwrap_or_else(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "cannot build stream",
            )
        })
}

struct Conv {
    up: Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    parser: SseParser,
    from: Dialect,
    to: Dialect,
    model: String,
    pending: VecDeque<Bytes>,
    dec_openai: crate::protocol::openai::InState,
    dec_anthropic: crate::protocol::anthropic::InState,
    enc_openai: crate::protocol::openai::OutState,
    enc_anthropic: crate::protocol::anthropic::OutState,
    done: bool,
}

fn convert_stream(
    up: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    from: Dialect,
    to: Dialect,
    model: String,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
    let state = Conv {
        up: Box::pin(up),
        parser: SseParser::default(),
        from,
        to,
        enc_openai: crate::protocol::openai::OutState::new(format!(
            "chatcmpl-{}",
            crate::config::random_key(20)
        )),
        enc_anthropic: crate::protocol::anthropic::OutState::new(format!(
            "msg_{}",
            crate::config::random_key(20)
        )),
        model,
        pending: VecDeque::new(),
        dec_openai: Default::default(),
        dec_anthropic: Default::default(),
        done: false,
    };

    stream::unfold(state, |mut st| async move {
        loop {
            if let Some(b) = st.pending.pop_front() {
                return Some((Ok::<Bytes, std::io::Error>(b), st));
            }
            if st.done {
                return None;
            }
            match st.up.next().await {
                Some(Ok(chunk)) => {
                    let events = st.parser.push(&chunk);
                    for ev in events {
                        if ev.is_done() {
                            continue;
                        }
                        st.feed(&ev);
                    }
                }
                Some(Err(e)) => {
                    st.done = true;
                    let msg = format!("data: {{\"error\":{{\"message\":\"{}\"}}}}\n\n", e);
                    return Some((Ok(Bytes::from(msg)), st));
                }
                None => {
                    st.done = true;
                    if let Some(ev) = st.parser.finish() {
                        st.feed(&ev);
                    }
                    st.finish();
                }
            }
        }
    })
}

impl Conv {
    fn feed(&mut self, ev: &crate::protocol::SseEvent) {
        let deltas: Vec<Delta> = match self.from {
            Dialect::Openai => {
                crate::protocol::openai::parse_stream(&ev.data, &mut self.dec_openai)
            }
            Dialect::Anthropic => {
                crate::protocol::anthropic::parse_stream(ev, &mut self.dec_anthropic)
            }
            Dialect::Gemini => crate::protocol::gemini::parse_stream(&ev.data),
        };
        for d in deltas {
            let frame = match self.to {
                Dialect::Openai => {
                    crate::protocol::openai::render_delta(&d, &mut self.enc_openai, &self.model)
                }
                Dialect::Anthropic => crate::protocol::anthropic::render_delta(
                    &d,
                    &mut self.enc_anthropic,
                    &self.model,
                ),
                Dialect::Gemini => None,
            };
            if let Some(f) = frame {
                self.pending.push_back(Bytes::from(f));
            }
        }
    }

    fn finish(&mut self) {
        match self.to {
            Dialect::Openai => {
                self.pending
                    .push_back(Bytes::from(crate::protocol::openai::finish_stream(
                        &self.enc_openai,
                    )));
            }
            Dialect::Anthropic => {
                self.pending
                    .push_back(Bytes::from(crate::protocol::anthropic::finish_stream()));
            }
            Dialect::Gemini => {}
        }
    }
}

/// Read the config directory out of the standard magpie environment variables,
/// so the Android shell can reuse exactly what the desktop build uses.
pub fn options_from_env() -> GatewayOptions {
    let mut opts = GatewayOptions::default();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let config_home =
        std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    opts.config_dir = std::path::PathBuf::from(config_home).join("magpie");

    if let Ok(addr) = std::env::var("MAGPIE_ADDR") {
        opts.gateway_addr = addr;
    }
    if let Ok(key) = std::env::var("MAGPIE_WEB_KEY")
        && !key.is_empty()
    {
        opts.web_key = Some(key);
    }
    if let Ok(web) = std::env::var("MAGPIE_WEB") {
        if web == "0" || web.eq_ignore_ascii_case("off") {
            opts.web_addr = None;
        } else {
            opts.web_addr = Some(web);
        }
    }
    opts.log_path = Some(opts.config_dir.join("magpie.log"));
    opts
}

/// Convenience for the CLI and for the Android shell: build, then run forever.
pub async fn serve(opts: GatewayOptions) -> std::io::Result<()> {
    let gw = Gateway::new(opts)?;
    gw.run(std::future::pending::<()>()).await
}

/// Parse a `host:port` string the way `MAGPIE_ADDR` does.
pub fn parse_addr(addr: &str) -> Result<SocketAddr, String> {
    use std::net::ToSocketAddrs;
    addr.to_socket_addrs()
        .map_err(|e| format!("{addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("{addr} resolves to nothing"))
}
