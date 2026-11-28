//! Floe's Postgres server end to end: an in-process server on 127.0.0.1:0, the floe-testkit
//! `sales.orders` table, and tokio-postgres as the client.

mod common;
mod protocol;
mod spans;
