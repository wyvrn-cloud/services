//! Who a request is from, for the limits that count by address: one rule for the
//! DIDComm endpoint, live connections and the web view alike, whether or not the
//! web view is on (security review WYV-98), by `wyvrn_service::client_address`.
//!
//! `X-Forwarded-For` is believed only from the trusted proxies (`[server]
//! trusted_proxies`). A request that comes from one of them with no client
//! forwarded is the proxy's own, or this machine's (a test, a tool beside it), and
//! isn't counted: counting it would put every client behind it in one bucket.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::HeaderMap;
use wyvrn_service::client_address::{bucket, docker_gateways, Proxies};

/// The proxies a community believes when none are set: the Docker networks'
/// gateways (as community-host has it), and this machine (a proxy beside it).
pub fn default_proxies() -> Vec<String> {
    let mut proxies = docker_gateways();
    proxies.extend(["127.0.0.1".to_string(), "::1".to_string()]);
    proxies
}

#[derive(Debug)]
pub struct Clients {
    proxies: Proxies,
    warned: AtomicBool,
}

impl Default for Clients {
    fn default() -> Self {
        Self::new(&default_proxies()).expect("the default proxies parse")
    }
}

impl Clients {
    pub fn new<S: AsRef<str>>(proxies: &[S]) -> anyhow::Result<Self> {
        Ok(Self { proxies: Proxies::parse(proxies).map_err(|e| anyhow::anyhow!("trusted_proxies: {e}"))?, warned: AtomicBool::new(false) })
    }

    /// The bucket a request from `peer` with `headers` is counted in, an IPv6
    /// client by its /64; `None` for one not to count (a trusted proxy's own).
    pub fn counted(&self, peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
        let client = self.proxies.client(peer, headers);
        if self.proxies.trusts(client) {
            return None;
        }
        if client == peer && is_local(peer) && !self.warned.swap(true, Ordering::Relaxed) {
            // Most likely a proxy nobody said to trust: then every client is this one.
            tracing::warn!(%peer, "requests come from a private address that isn't a trusted proxy, so every client behind it is counted as one; if it is your reverse proxy, add it to [server] trusted_proxies (COMMUNITY_TRUSTED_PROXIES)");
        }
        Some(bucket(client))
    }

    /// The same, from a request.
    pub fn of(&self, request: &axum::extract::Request) -> Option<IpAddr> {
        let peer = request.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|info| info.0.ip())?;
        self.counted(peer, request.headers())
    }
}

fn is_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local(IpAddr::V4(v4)),
            None => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forwarded(xff: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", xff.parse().unwrap());
        headers
    }

    #[test]
    fn clients_behind_the_default_proxies_are_counted_each_and_the_proxy_not_at_all() {
        let clients = Clients::default();
        let gateway: IpAddr = "172.18.0.1".parse().unwrap();
        // Through the gateway: the client it forwarded for, by its /64.
        assert_eq!(clients.counted(gateway, &forwarded("2001:db8:1:2::9")), Some("2001:db8:1:2::".parse().unwrap()));
        assert_eq!(clients.counted(gateway, &forwarded("203.0.113.5")), Some("203.0.113.5".parse().unwrap()));
        // The proxy itself, or this machine, with nobody forwarded: not counted.
        assert_eq!(clients.counted(gateway, &HeaderMap::new()), None);
        assert_eq!(clients.counted("127.0.0.1".parse().unwrap(), &HeaderMap::new()), None);
        // A proxy beside it on this machine is believed too.
        assert_eq!(clients.counted("127.0.0.1".parse().unwrap(), &forwarded("198.51.100.7")), Some("198.51.100.7".parse().unwrap()));
        // Anyone else is who they are, whatever they say.
        let other: IpAddr = "172.18.0.5".parse().unwrap();
        assert_eq!(clients.counted(other, &forwarded("203.0.113.5")), Some(other));
    }

    #[test]
    fn a_list_without_this_machine_counts_it() {
        let clients = Clients::new(&["10.0.0.1"]).unwrap();
        assert_eq!(clients.counted("127.0.0.1".parse().unwrap(), &HeaderMap::new()), Some("127.0.0.1".parse().unwrap()));
        assert!(Clients::new(&["nonsense"]).is_err());
    }
}
