//! Logging, set up the same way by every service.

/// Logs to stderr at `info`, or as `RUST_LOG` says.
pub fn init() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
}
