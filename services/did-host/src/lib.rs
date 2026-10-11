//! wyvrn-did-host: hosts short, stable `did:webvh` DIDs, and their parallel
//! `did:web`, for agents that write and sign them themselves. It speaks
//! `https://wyvrn.app/did-hosting/1.0`.
//!
//! - [`names`]: what a name is, and which are set aside.
//! - [`log`]: checking a `did:webvh` log.
//! - [`keys`]: the keys in a document, and a signature against them.
//! - [`store`]: what is kept for each name.
//! - [`host`]: the protocol -- what each message does.
//! - [`server`]: the HTTP side: the endpoint messages come to, and serving what is hosted.

pub mod config;
pub mod host;
pub mod keys;
pub mod log;
pub mod names;
pub mod server;
pub mod store;
