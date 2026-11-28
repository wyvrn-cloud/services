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
//! - [`creating`]: making a community from the app, as the host's admins allow.
//! - [`backups`]: each community backed up on a schedule, sealed with a key.
//! - [`buckets`]: where each community's attached files are kept: the host's
//!   bucket, its owner's own, or the disk.
//! - [`export`]: a community's owner moving it to a server of their own.
//! - [`import`]: the reverse, a community run on its own brought onto the host.
//! - [`expressions`]: a Discord server's own emoji and stickers made a community's packs.
//! - [`history`]: a Discord server's history (DiscordChatExporter's JSON) brought into a community.
//!
//! A community that outgrows its host moves to a server of its own, the single
//! `wyvrn-community`, keeping its keys and public address.

pub mod admin;
pub mod backups;
pub mod buckets;
pub mod config;
pub mod creating;
pub mod domains;
pub mod export;
pub mod expressions;
pub mod history;
pub mod import;
pub mod registry;
pub mod server;
pub mod tenants;
