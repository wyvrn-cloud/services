//! A files service: the `host` role of `files/1.0`. Agents it allows put files
//! here over HTTPS, in chunks that resume, and hand them to anyone as a DIDComm
//! attachment by reference. It keeps bytes; it doesn't look at them.

pub mod config;
pub mod hash;
pub mod host;
pub mod policy;
pub mod server;
pub mod store;
