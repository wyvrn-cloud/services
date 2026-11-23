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

use crate::{Incoming, Service};

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
    let received = match agent.receive(&body).await {
        Ok(r) => r,
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
    let sender_kid = match agent.messaging().unpack_verified(&body).await {
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
    match agent.respond(&received, &reply).await {
        Ok(Some(packed)) => ([(header::CONTENT_TYPE, "application/didcomm-encrypted+json")], packed).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        // The detail is logged, never sent: it carries what the sender's endpoint
        // answered, and that endpoint can be any address (redirected to one), so
        // echoing it would read an internal response back to a stranger (security
        // review WYV-92).
        Err(e) => {
            tracing::warn!(error = %e, "couldn't deliver reply");
            (StatusCode::BAD_GATEWAY, DELIVERY_FAILED).into_response()
        }
    }
}

/// Serves `router` at `bind` until the process is interrupted (Ctrl-C, or the
/// signal a container is stopped with), letting requests in flight finish.
pub async fn serve(bind: &str, router: Router) -> anyhow::Result<()> {
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
