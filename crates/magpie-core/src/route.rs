//! Model → provider resolution.
//!
//! Upstream magpie has a whole routing engine (`internal/gateway/routing.go`,
//! strategies `smart`/`order`/`rotate`/`usage`/`pace`, nested groups, sticky
//! sessions). magpie-rs keeps the part an Android user actually reaches for:
//! explicit `provider/model` names, a declared model catalogue, and three
//! selection modes.

use crate::config::{Config, Protocol, Provider, RouteMode};

/// One provider that can serve the requested model.
#[derive(Debug, Clone)]
pub struct Target {
    pub provider_id: String,
    pub provider_name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: String,
    pub headers: Vec<(String, String)>,
    /// The model string to put on the wire.
    pub upstream_model: String,
}

#[derive(Debug)]
pub enum RouteError {
    /// Nothing serves this model.
    NotFound(String),
    /// The model exists but every provider that serves it is disabled.
    Disabled(String),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::NotFound(m) => write!(
                f,
                "model `{m}` is not served by any configured provider; add it in the web UI"
            ),
            RouteError::Disabled(m) => {
                write!(f, "every provider that serves `{m}` is disabled")
            }
        }
    }
}

fn target(p: &Provider, upstream_model: String) -> Target {
    Target {
        provider_id: p.id.clone(),
        provider_name: p.display().to_string(),
        protocol: p.protocol,
        base_url: p.base().to_string(),
        api_key: p.api_key.clone(),
        headers: p
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        upstream_model,
    }
}

/// Resolve `requested` to the ordered list of providers to try.
///
/// Order is: lowest `priority` first, declaration order breaking ties. Under
/// [`RouteMode::Rotate`] the list is rotated so the first entry changes per
/// request; under every mode the full list is returned because the caller
/// retries on 5xx.
pub fn resolve(cfg: &Config, requested: &str) -> Result<Vec<Target>, RouteError> {
    let requested = requested.trim();

    // 1. Explicit `provider/model`.
    if let Some((prefix, rest)) = requested.split_once('/')
        && let Some(p) = cfg.provider(prefix)
    {
        if !p.enabled {
            return Err(RouteError::Disabled(requested.to_string()));
        }
        return Ok(vec![target(p, rest.to_string())]);
    }

    // 2. A provider that declares this exact model.
    let mut matches: Vec<&Provider> = cfg
        .enabled_providers()
        .filter(|p| p.models.iter().any(|m| m == requested))
        .collect();
    matches.sort_by_key(|p| p.priority);
    if !matches.is_empty() {
        let mut targets: Vec<Target> = matches
            .into_iter()
            .map(|p| target(p, requested.to_string()))
            .collect();
        if cfg.settings.route == RouteMode::Rotate {
            let idx = cfg.next_rotate(targets.len());
            targets.rotate_left(idx);
        }
        return Ok(targets);
    }

    // 3. The model is unknown but exactly one provider is a catch-all (no
    //    declared models), which is how a private endpoint is configured.
    let catch_all: Vec<&Provider> = cfg
        .enabled_providers()
        .filter(|p| p.models.is_empty())
        .collect();
    if catch_all.len() == 1 {
        return Ok(vec![target(catch_all[0], requested.to_string())]);
    }
    if catch_all.len() > 1 {
        let mut targets: Vec<Target> = catch_all
            .into_iter()
            .map(|p| target(p, requested.to_string()))
            .collect();
        targets.sort_by_key(|t| {
            cfg.providers
                .iter()
                .find(|p| p.id == t.provider_id)
                .map(|p| p.priority)
                .unwrap_or(0)
        });
        return Ok(targets);
    }

    if cfg
        .providers
        .iter()
        .any(|p| !p.enabled && p.models.iter().any(|m| m == requested))
    {
        return Err(RouteError::Disabled(requested.to_string()));
    }
    Err(RouteError::NotFound(requested.to_string()))
}

/// The catalogue for `GET /v1/models`: both `provider/model` and every bare
/// model name that exactly one provider serves.
pub fn catalogue(cfg: &Config) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for p in cfg.enabled_providers() {
        if p.models.is_empty() {
            continue;
        }
        for m in &p.models {
            let bare_count = cfg
                .enabled_providers()
                .filter(|q| q.models.iter().any(|x| x == m))
                .count();
            out.push(serde_json::json!({
                "id": format!("{}/{}", p.id, m),
                "object": "model",
                "owned_by": p.id,
                "created": 0,
            }));
            if bare_count == 1 {
                out.push(serde_json::json!({
                    "id": m,
                    "object": "model",
                    "owned_by": p.id,
                    "created": 0,
                }));
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|v| seen.insert(v["id"].as_str().unwrap_or_default().to_string()));
    out
}
