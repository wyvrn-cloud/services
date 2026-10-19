//! An outbound relay: the `relay` role of `outbound-relay/1.0`. A client holds a
//! WebSocket open to it and hands it what it would post to others' endpoints;
//! the relay posts it, so that where it goes sees the relay and not the client.
//! Set to carry `all`, it also hands back answers and carries a WebSocket, which
//! is what a client's own mediator traffic needs.

pub mod config;
pub mod guard;
pub mod relay;
pub mod server;
pub mod store;
