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

use crate::Service;

/// Something went wrong that is not the caller's doing: logged, and not described
/// to them.
pub fn internal(error: impl std::fmt::Display) -> Response {
    tracing::error!(%error, "request failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "something went wrong").into_response()
}

/// A packed DIDComm message posted to the service: unpacked, answered by the
/// service, and the answer sent back -- in the response, or to the sender's own
/// address if that is how they asked to be answered.
///
/// For a service's `POST` handler: `|State(state), body| receive(state, body)`.
pub async fn receive<S: Service>(service: Arc<S>, body: Bytes) -> Response {
    let agent = service.agent();
    let received = match agent.receive(&body).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("unpack failed: {e}")).into_response(),
    };
    tracing::debug!(sender = ?received.sender, message_type = received.message_type(), "received");
    let reply = match service.answer(&received).await {
        Ok(Some(reply)) => reply,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(e) => return internal(e),
    };
    match agent.respond(&received, &reply).await {
        Ok(Some(packed)) => ([(header::CONTENT_TYPE, "application/didcomm-encrypted+json")], packed).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't deliver reply");
            (StatusCode::BAD_GATEWAY, format!("couldn't deliver reply: {e}")).into_response()
        }
    }
}

/// Serves `router` at `bind` until the process is interrupted (Ctrl-C, or the
/// signal a container is stopped with), letting requests in flight finish.
pub async fn serve(bind: &str, router: Router) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("binding {bind}"))?;
    axum::serve(listener, router).with_graceful_shutdown(stop_signal()).await?;
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
