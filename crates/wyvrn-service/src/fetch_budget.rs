//! DID documents fetched on behalf of an incoming message, charged to the address
//! it came from (security review WYV-73).
//!
//! An authcrypted message names its sender's key, and that key's DID is resolved
//! before anything is decrypted, so anyone can make a service fetch a did:web or
//! did:webvh document. The library asks a process-wide gate before each fetch it
//! would make (`didcomm_core::resolver::set_fetch_gate`; never for what it answers
//! from its cache or locally); this module is that gate. A fetch made while
//! opening a message is charged to the message's client, by
//! [`client_address::bucket`], at most [`FETCHES_PER_MINUTE`] a minute; past that
//! the message is refused with 429 (or, where there's no HTTP answer, the frame is
//! dropped), not taken as one that couldn't be opened. Nothing else is charged: a
//! service's own outbound work, and messages it collects from its mediator (which
//! limits who may send), go through.
//!
//! How it is wired: [`note_client`] notes each HTTP request's client in a task-local,
//! [`charged`] scopes the charging around an `unpack`, and [`install`] sets the gate
//! once; [`crate::http::serve_with`] does the first and the last for a service.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::client_address::{bucket, Proxies};

/// How many DID documents one client may make a service fetch in a minute. A
/// fetch is only what isn't cached, so an honest sender costs one per sender DID
/// it uses, once an hour or so.
pub const FETCHES_PER_MINUTE: u32 = 30;

/// The most clients remembered at once; the oldest window is let go past it.
const MAX_CLIENTS: usize = 10_000;

const WINDOW: Duration = Duration::from_secs(60);

tokio::task_local! {
    /// The client an HTTP request is from, as [`note_client`] found it (`None`: a
    /// trusted proxy's own request, not counted).
    static REQUEST_CLIENT: Option<IpAddr>;
    /// Whom a fetch made now is charged to.
    static CHARGED_TO: Option<IpAddr>;
}

/// The client of the HTTP request being handled, if [`note_client`] is in front of it.
pub fn request_client() -> Option<IpAddr> {
    REQUEST_CLIENT.try_with(|client| *client).ok().flatten()
}

/// Runs `work` (an `unpack` of what `client` sent) with any DID fetch it makes
/// charged to `client`, already a [`bucket`]. `None` charges nobody.
pub async fn charged<F: Future>(client: Option<IpAddr>, work: F) -> F::Output {
    CHARGED_TO.scope(client, work).await
}

/// Middleware noting each request's client for [`request_client`], by `proxies`'
/// rule (`client_address`): `axum::middleware::from_fn_with_state(proxies, note_client)`.
pub async fn note_client(State(proxies): State<Arc<Proxies>>, request: Request, next: Next) -> Response {
    let client = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| proxies.client(info.0.ip(), request.headers()))
        .filter(|client| !proxies.trusts(*client))
        .map(bucket);
    REQUEST_CLIENT.scope(client, next.run(request)).await
}

#[derive(Default)]
struct Windows {
    clients: HashMap<IpAddr, (Instant, u32)>,
}

impl Windows {
    fn take(&mut self, client: IpAddr, now: Instant, limit: u32) -> bool {
        if !self.clients.contains_key(&client) && self.clients.len() >= MAX_CLIENTS {
            self.clients.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
            if self.clients.len() >= MAX_CLIENTS {
                if let Some(oldest) = self.clients.iter().min_by_key(|(_, (start, _))| *start).map(|(c, _)| *c) {
                    self.clients.remove(&oldest);
                }
            }
        }
        let entry = self.clients.entry(client).or_insert((now, 0));
        if now.duration_since(entry.0) >= WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

static INSTALL: Once = Once::new();

/// Sets the library's fetch gate to this budget, once a process (later calls do
/// nothing).
pub fn install() {
    INSTALL.call_once(|| {
        let windows = Mutex::new(Windows::default());
        didcomm_core::resolver::set_fetch_gate(Some(Arc::new(move |_did: &str| {
            let Some(client) = CHARGED_TO.try_with(|client| *client).ok().flatten() else {
                return Ok(());
            };
            let allowed = windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(client, Instant::now(), FETCHES_PER_MINUTE);
            if allowed {
                Ok(())
            } else {
                Err(format!("{client} has had {FETCHES_PER_MINUTE} DID documents fetched this minute"))
            }
        })));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_gets_its_fetches_a_minute_and_then_none_until_the_next() {
        let mut windows = Windows::default();
        let (a, b) = ("192.0.2.1".parse().unwrap(), "192.0.2.2".parse().unwrap());
        let start = Instant::now();
        for _ in 0..3 {
            assert!(windows.take(a, start, 3));
        }
        assert!(!windows.take(a, start + Duration::from_secs(59), 3));
        assert!(windows.take(b, start, 3), "another client has its own");
        assert!(windows.take(a, start + WINDOW, 3), "a new minute");
    }

    #[test]
    fn the_clients_remembered_are_bounded() {
        let mut windows = Windows::default();
        let now = Instant::now();
        for n in 0..(MAX_CLIENTS as u32 + 5) {
            windows.take(IpAddr::from(n.to_be_bytes()), now, 1);
        }
        assert!(windows.clients.len() <= MAX_CLIENTS);
    }

    #[tokio::test]
    async fn only_a_charged_fetch_counts() {
        install();
        let client: IpAddr = "198.51.100.7".parse().unwrap();
        for _ in 0..FETCHES_PER_MINUTE {
            assert!(charged(Some(client), async { didcomm_core::resolver::check_fetch("did:web:example.com") }).await.is_ok());
        }
        let refused = charged(Some(client), async { didcomm_core::resolver::check_fetch("did:web:example.com") }).await;
        assert!(refused.unwrap_err().is_fetch_refused());
        assert!(didcomm_core::resolver::check_fetch("did:web:example.com").is_ok(), "the service's own fetch isn't charged");
        assert!(charged(None, async { didcomm_core::resolver::check_fetch("did:web:example.com") }).await.is_ok());
    }
}
