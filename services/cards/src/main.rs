use std::sync::Arc;

use wyvrn_cards::{config::Config, server, who::Https};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    wyvrn_service::logging::init();
    let config = Config::from_env();
    tracing::info!(bind = %config.bind, site = %config.site_name, did_host = %config.did_host, "link cards ready");
    let bind = config.bind.clone();
    let state = server::AppState::new(config, Arc::new(Https::new()?));
    wyvrn_service::http::serve(&bind, server::router(state)).await
}
