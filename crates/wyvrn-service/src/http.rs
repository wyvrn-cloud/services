//! The HTTP side every service has: the endpoint DIDComm messages are posted to,
//! and serving until told to stop.

use std::sync::Arc;

use anyhow::Context;
use axum::{
    body::Bytes,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Router,
};

use crate::{client_address::Proxies, Incoming, Service};

/// Something went wrong that is not the caller's doing: logged, and not described
/// to them.
pub fn internal(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "something went wrong").into_response()
}

/// What a sender is told when its reply couldn't be delivered: fixed, whatever
/// the reason (security review WYV-92).
pub const DELIVERY_FAILED: &str = "couldn't deliver the reply";

/// A packed DIDComm message posted to the service: unpacked, answered by the
/// service, and the answer sent back -- in the response, or to the sender's own
/// address if that is how they asked to be answered.
///
/// For a service's `POST` handler: `|State(state), body| receive(state, body)`.
pub async fn receive<S: Service>(service: Arc<S>, body: Bytes) -> Response {
    let agent = service.agent();
    // DID documents fetched to open it are charged to whoever sent it (WYV-73).
    let client = crate::fetch_budget::request_client();
    let received = match crate::fetch_budget::charged(client, agent.receive(&body)).await {
        Ok(r) => r,
        // Not a message that couldn't be opened: this address has had its share of
        // fetches this minute (DIDComm v2.1 over HTTPS: a 4xx the sender can act on).
        Err(e) if e.is_fetch_refused() => {
            tracing::debug!(error = %e, "a message refused for its address's DID fetches");
            return (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "60")], "too many DID lookups from your address; try again in a minute").into_response();
        }
        // Said in the log, not to the caller: what went wrong unpacking can be what
        // a fetch of the sender's document met, which is no stranger's to learn
        // (security review WYV-15).
        Err(e) => {
            tracing::debug!(error = %e, "a message that didn't unpack");
            return (StatusCode::BAD_REQUEST, "that message couldn't be opened").into_response();
        }
    };
    tracing::debug!(sender = ?received.sender, message_type = received.message_type(), "received");
    // Which key it was authenticated with: `receive` says whose, and a service
    // that decides by the key (not every key of a DID is the same party's word)
    // needs to know which. Opened a second time to learn it; the first already
    // established that it opens.
    let sender_kid = match crate::fetch_budget::charged(client, agent.messaging().unpack_verified(&body)).await {
        Ok(unpacked) => unpacked.sender_kid,
        Err(_) => None,
    };
    if service.requires_authcrypt() && sender_kid.is_none() {
        // There is nobody to answer in DIDComm: say it where the sender will see it.
        return (StatusCode::FORBIDDEN, "this service only takes messages their sender has authenticated (authcrypt)").into_response();
    }
    let reply = match service.answer(&Incoming { received: &received, sender_kid }).await {
        Ok(Some(reply)) => reply,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(e) => return internal(e),
    };
    // Asked for on this connection, the reply is the response: packed, not sent.
    if received.wants_reply_on_connection() {
        return match agent.pack_reply(&received, &reply).await {
            Ok(packed) => ([(header::CONTENT_TYPE, "application/didcomm-encrypted+json")], packed).into_response(),
            Err(e) => internal(e),
        };
    }
    // Sent to the sender's own address, after they have been told it was taken:
    // their request isn't held open while the reply crosses to their mediator
    // (asked 2026-11-26: a post to the updates channel, taken, timed out in the
    // app while its reply was on the way, up to 30 s behind the channel's own
    // sending to its subscribers). Whatever the sender's endpoint answers is only
    // logged, never sent back (security review WYV-92).
    tokio::spawn(async move {
        if let Err(e) = service.agent().respond(&received, &reply).await {
            tracing::warn!(error = %e, "{DELIVERY_FAILED}");
        }
    });
    StatusCode::ACCEPTED.into_response()
}

/// Serves `router` at `bind` until the process is interrupted (Ctrl-C, or the
/// signal a container is stopped with), letting requests in flight finish.
///
/// A request's client is the connection's address, or behind one of the Docker
/// networks' gateways what it forwarded ([`client_address::docker_gateways`]); a
/// service told which proxies are in front of it uses [`serve_with`].
pub async fn serve(bind: &str, router: Router) -> anyhow::Result<()> {
    serve_with(bind, router, Proxies::parse(&crate::client_address::docker_gateways())?).await
}

/// [`serve`], with `proxies` the ones whose `X-Forwarded-For` is believed. DID
/// documents fetched to open a posted message are charged to its client
/// ([`crate::fetch_budget`], WYV-73).
pub async fn serve_with(bind: &str, router: Router, proxies: Proxies) -> anyhow::Result<()> {
    crate::fetch_budget::install();
    let router = router.layer(axum::middleware::from_fn_with_state(Arc::new(proxies), crate::fetch_budget::note_client));
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("binding {bind}"))?;
    // With who each connection is from, for whatever limits requests by address.
    axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(stop_signal()).await?;
    Ok(())
}

async fn stop_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        // What `docker stop` sends. Without it a container is killed after its
        // grace period instead of stopping when asked.
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = interrupt => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => interrupt.await,
        }
    }
    #[cfg(not(unix))]
    interrupt.await;
}
