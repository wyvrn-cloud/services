//! A live connection's frames count against its address's messages a minute, as
//! posts do (security review WYV-97). A binary of its own: the count is the
//! process's.

use std::sync::Arc;
use std::time::Duration;

use didcomm_agent::Identity;
use futures_util::{SinkExt, StreamExt};
use wyvrn_community::{
    community::Community,
    server::{self, AppState},
    store::Store,
};

#[tokio::test]
async fn frames_past_the_limit_close_the_connection_with_try_again_later() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let config: wyvrn_community::config::Config = toml::from_str(&format!(
        "[server]\nbind = \"unused\"\npublic_url = \"{url}\"\ntrusted_proxies = [\"10.9.9.9\"]\n[identity]\npath = \"unused\"\n[store]\npath = \"unused\"\n[community]\nname = \"The Workshop\"\n"
    ))
    .unwrap();
    let (agent, did_document) = server::build_agent(&config, Identity::generate().unwrap()).unwrap();
    let community = Community::new(Store::in_memory().unwrap(), config.community.clone(), Some(wyvrn_service::profile::Kept::scratch().unwrap())).unwrap();
    let mut state = AppState::new(agent, Default::default(), community, did_document, didcomm_quickstart::default_resolver());
    // This machine isn't one of its proxies here, so it is counted like any client.
    state.clients = wyvrn_community::clients::Clients::new(&config.trusted_proxies()).unwrap();
    let app = server::router(Arc::new(state));
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });

    let response = reqwest_websocket::Upgrade::upgrade(reqwest::Client::new().get(url.replacen("http://", "ws://", 1))).send().await.unwrap();
    let mut socket = response.into_websocket().await.unwrap();
    // Not even DIDComm: counted before anything is opened.
    for n in 0..301 {
        if socket.send(reqwest_websocket::Message::Text(format!("{{\"junk\":{n}}}"))).await.is_err() {
            break;
        }
    }
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next()).await.expect("closed within ten seconds");
        match frame {
            Some(Ok(reqwest_websocket::Message::Close { code, .. })) => {
                assert_eq!(u16::from(code), 1013);
                break;
            }
            Some(Ok(_)) => continue,
            other => panic!("closed without saying why: {other:?}"),
        }
    }
}
