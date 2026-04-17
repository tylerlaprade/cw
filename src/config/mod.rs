//! Config loading. `.devcli.toml` in repo root (optional). When absent, all
//! values come from autodetection. Discovery walks up from cwd until git root.

pub mod command;
pub mod discover;
pub mod init;
pub mod schema;

pub use schema::Config;
