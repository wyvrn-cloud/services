//! A one-way announcement channel over DIDComm
//! ([`https://wyvrn.app/announcements/1.0`](channel::PIURI)): people subscribe, admins
//! post, every subscriber receives it, and a web page shows the whole log.

pub mod channel;
pub mod config;
pub mod server;
pub mod store;
pub mod web;
