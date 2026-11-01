//! Many community servers in one process, as ACA-Py keeps many agents: each
//! community its own DID, keys and stores, and nothing of one reaching another.
//!
//! - [`tenants`]: opening a community when it is spoken to, closing it when idle,
//!   and handing it its requests.
//! - [`registry`]: which communities there are, and where each is kept: a folder
//!   of SQLite files, a MySQL database, or tables of its own in a shared one.
//! - [`server`]: the host's own DID, each community under `/c/<name>/`, and the
//!   voice relay's question passed to the community a token is of.
//! - [`admin`]: the host's admin pages.
//!
//! A community that outgrows its host moves to a server of its own, the single
//! `wyvrn-community`, keeping its keys and public address.

pub mod admin;
pub mod config;
pub mod registry;
pub mod server;
pub mod tenants;
