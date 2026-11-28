//! Serves Iceblink over the Postgres wire protocol: startup, simple and extended queries, and
//! errors with the matching SQLSTATE, on top of datafusion-postgres and pgwire.
//!
//! It must never run a statement outside the allow-list: SELECT, EXPLAIN, SHOW, and SET for
//! approved session settings only. Every statement gets one tracing span saying what it read.

mod allow;
mod connection;
mod drain;
mod error;
mod errors;
mod fingerprint;
mod handler;
mod hook;
mod server;
mod setting;
mod settings;
mod statement;

pub use error::PgError;
pub use server::{ShutdownSummary, serve, serve_on};
pub use settings::ServerSettings;
