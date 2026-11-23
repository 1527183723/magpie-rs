//! magpie-core — the gateway half of magpie-rs.
//!
//! This crate is the Rust reimplementation of what the upstream Go project
//! (`yetone/magpie`) calls `internal/gateway` plus the parts of
//! `internal/provider` that the gateway needs: it speaks the OpenAI, Anthropic
//! and Gemini wire formats, translates between them, and routes each request to
//! whichever upstream provider owns the requested model.
//!
//! It is deliberately free of any Android dependency so the same code runs on a
//! desktop for testing (`cargo run -p magpie-core`) and inside the APK.

pub mod config;
pub mod gateway;
pub mod ir;
pub mod log;
pub mod protocol;
pub mod route;
pub mod usage;
pub mod web;

pub use config::{Config, Protocol, Provider, Settings};
pub use gateway::{Gateway, GatewayOptions};
