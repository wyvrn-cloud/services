//! What every Wyvrn DIDComm service does, whatever it is for.
//!
//! A service is an agent with an address, something that answers the messages it
//! is sent, and usually a web side of its own. This crate is the first two and the
//! plumbing between them:
//!
//! - [`address`]: the service's DID -- a `did:web` at its public URL, or a
//!   `did:peer:4` -- and the routes that publish it.
//! - [`Service`]: the one thing a service writes -- what to answer a message with.
//! - [`http`]: the endpoint messages are posted to, and serving until told to stop.
//! - [`mediator`]: being reached through a mediator instead, for a service with no
//!   address of its own that the people it serves can get to.
//! - [`logging`], [`now`]: the same start-up and the same clock for all of them.
//!
//! A service keeps what is its own: its configuration file, its store, its
//! protocol, its pages.

pub mod address;
pub mod http;
pub mod logging;
pub mod mediator;

use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

use didcomm_agent::{Agent, Received};
use serde_json::Value;

/// A message that has arrived, and what is known of how.
pub struct Incoming<'a> {
    pub received: &'a Received,
    /// The id of the key the sender authenticated with (`did:…#key-2`): *which* of
    /// its DID's keys it holds, not only that it holds one. `None` for a message
    /// nobody authenticated (anoncrypt). The same whether it was posted to the
    /// endpoint or carried by a mediator: a mediator hands on the sender's sealed
    /// message, and it is opened here either way.
    pub sender_kid: Option<String>,
}

/// A service, as the plumbing here sees it: an agent, and what it answers.
pub trait Service: Send + Sync + 'static {
    fn agent(&self) -> &Agent;

    /// Whether the service takes only messages whose sender authenticated them
    /// with a key of its own (authcrypt). One that does is never handed anything
    /// else: an anonymous message posted to it is refused there and then, and one
    /// a mediator carried is dropped.
    fn requires_authcrypt(&self) -> bool {
        false
    }

    /// Acts on one message, however it got here (posted to the endpoint, or
    /// collected from a mediator), and returns what to answer it with, if anything.
    fn answer(&self, incoming: &Incoming<'_>) -> impl Future<Output = anyhow::Result<Option<Value>>> + Send;
}

/// Now, in seconds since the epoch: what DIDComm's `created_time` counts in.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}
