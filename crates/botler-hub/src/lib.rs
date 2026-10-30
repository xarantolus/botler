//! The botler hub.

pub mod agents;
pub mod api;
pub mod assets;
pub mod auth;
pub mod bindings;
pub mod config;
pub mod control;
pub mod embedded;
pub mod failures;
pub mod hub;
pub mod images;
pub mod jobs;
pub mod live;
pub mod mcp;
pub mod notify;
pub mod ops;
pub mod phase;
pub mod plumbing;
pub mod prompt;
pub mod router;
pub mod scheduler;
pub mod secret_requests;
pub mod serve;
pub mod sessions;
pub mod supervisor;
pub mod template;
pub mod transactions;
pub mod web;
pub mod wire;

pub use hub::{Hub, HubError};
