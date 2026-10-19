//! Where the relay may go. Anyone it grants could otherwise point it at what sits
//! behind its firewall (an admin page on this machine, a database on its network),
//! so it goes only to public addresses, and to the hosts its operator allows.
//!
//! Checked twice: an endpoint is looked at before anything is sent, so a refusal
//! can say why; and every address the HTTP client connects to comes through
//! [`Guard`]'s resolver, so a name that resolves to somewhere public when checked
//! and somewhere private a moment later (DNS rebinding) is still refused.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Url;

/// Whether `ip` is on the public internet: not this machine, not a private or
/// shared network, not link-local, multicast or reserved.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // Carrier-grade NAT (100.64.0.0/10), "this network" and the reserved block.
                || (a == 100 && (b & 0xc0) == 64)
                || a == 0
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local (fc00::/7) and link-local (fe80::/10).
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80)
        }
    }
}

#[derive(Clone, Default)]
pub struct Guard {
    /// Hosts and addresses the operator allows although they are private, lowercased.
    allow: Arc<Vec<String>>,
}

/// Why an endpoint was refused, in words for the client.
pub type Why = String;

impl Guard {
    pub fn new(allow: &[String]) -> Self {
        Self { allow: Arc::new(allow.iter().map(|host| host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase()).collect()) }
    }

    fn allowed(&self, host: &str) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
        self.allow.contains(&host)
    }

    /// `endpoint`, if it is one of `schemes` and goes somewhere the relay may go.
    pub async fn check(&self, endpoint: &str, schemes: &[&str]) -> Result<Url, Why> {
        let url = Url::parse(endpoint).map_err(|_| format!("{endpoint:?} isn't an address"))?;
        if !schemes.contains(&url.scheme()) {
            return Err(format!("the relay goes only to {} addresses", schemes.join(" and ")));
        }
        let host = url.host_str().ok_or_else(|| format!("{endpoint} has no host"))?.to_string();
        if self.allowed(&host) {
            return Ok(url);
        }
        let addresses: Vec<IpAddr> = match host.trim_start_matches('[').trim_end_matches(']').parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => tokio::net::lookup_host((host.as_str(), 0)).await.map_err(|_| format!("{host} couldn't be found"))?.map(|address| address.ip()).collect(),
        };
        if addresses.is_empty() || !addresses.iter().copied().all(is_public) {
            return Err(format!("{host} is on a private network or this machine"));
        }
        Ok(url)
    }
}

impl Resolve for Guard {
    fn resolve(&self, name: Name) -> Resolving {
        let guard = self.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let allowed = guard.allowed(&host);
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.filter(|address| allowed || is_public(address.ip())).collect();
            if addresses.is_empty() {
                return Err(format!("{host} is on a private network or this machine").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_local_addresses_are_not_public() {
        for private in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "192.168.1.1", "169.254.1.1", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1"] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[tokio::test]
    async fn an_endpoint_is_refused_for_its_scheme_or_a_private_address_unless_allowed() {
        let guard = Guard::new(&["10.0.0.5".to_string(), "mediator".to_string()]);
        assert!(guard.check("https://1.1.1.1/", &["http", "https"]).await.is_ok());
        assert!(guard.check("ftp://1.1.1.1/", &["http", "https"]).await.is_err());
        assert!(guard.check("http://127.0.0.1:8080/", &["http", "https"]).await.is_err());
        assert!(guard.check("http://localhost/", &["http", "https"]).await.is_err());
        assert!(guard.check("http://10.0.0.5:8080/", &["http", "https"]).await.is_ok());
        assert!(guard.check("ws://mediator:8080/ws", &["ws", "wss"]).await.is_ok());
        assert!(guard.check("wss://[::1]/", &["ws", "wss"]).await.is_err());
    }
}
