//! Where a service may go when someone else names the address (the relay's
//! endpoints, wyvrn-verify's Bluesky discovery). Whoever names it could otherwise
//! point it at what sits behind its firewall (an admin page on this machine, a
//! database on its network), so it goes only to public addresses, and to the hosts
//! its operator allows.
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
/// shared network, not link-local, multicast or reserved, and not an IPv6 form that
/// carries or translates an IPv4 address (NAT64, 6to4, Teredo, v4-compatible),
/// whose other end may be a private one (WYV-91). The same table of ranges is
/// tested in wyvrn-verify's and the app's guards.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // Carrier-grade NAT (100.64.0.0/10) and "this network" (0.0.0.0/8).
                || (a == 100 && (b & 0xc0) == 64)
                || a == 0
                // IETF protocol assignments (192.0.0.0/24), benchmarking (198.18.0.0/15).
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (b & 0xfe) == 18)
                // Reserved (240.0.0.0/4), and the broadcast address in it.
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // IPv4-compatible (::a.b.c.d, deprecated): the first 96 bits zero.
                || s[..6].iter().all(|&x| x == 0)
                // NAT64: the well-known prefix (64:ff9b::/96) and local use (64:ff9b:1::/48).
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0))
                || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
                // Discard-only (100::/64).
                || (s[0] == 0x100 && s[1..4].iter().all(|&x| x == 0))
                // Teredo (2001::/32), documentation (2001:db8::/32), 6to4 (2002::/16).
                || (s[0] == 0x2001 && s[1] == 0)
                || (s[0] == 0x2001 && s[1] == 0xdb8)
                || s[0] == 0x2002
                // Unique local (fc00::/7) and link-local (fe80::/10).
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80)
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

    /// Whether the operator allowed any private host at all (a test, a LAN).
    pub fn allows_any(&self) -> bool {
        !self.allow.is_empty()
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

    const NOT_PUBLIC: &[(&str, &str)] = &[
        ("0.0.0.0/8", "0.1.2.3"),
        ("10.0.0.0/8", "10.1.2.3"),
        ("100.64.0.0/10", "100.64.0.1"),
        ("127.0.0.0/8", "127.0.0.1"),
        ("169.254.0.0/16", "169.254.1.1"),
        ("172.16.0.0/12", "172.16.0.1"),
        ("192.0.0.0/24", "192.0.0.8"),
        ("192.0.2.0/24", "192.0.2.1"),
        ("192.168.0.0/16", "192.168.1.1"),
        ("198.18.0.0/15", "198.18.0.1"),
        ("198.18.0.0/15", "198.19.255.254"),
        ("198.51.100.0/24", "198.51.100.1"),
        ("203.0.113.0/24", "203.0.113.1"),
        ("224.0.0.0/4", "224.0.0.1"),
        ("240.0.0.0/4", "240.0.0.1"),
        ("240.0.0.0/4", "255.255.255.255"),
        ("::/128", "::"),
        ("::1/128", "::1"),
        ("::ffff:0:0/96 (mapped)", "::ffff:10.0.0.1"),
        ("::/96 (v4-compatible)", "::10.0.0.1"),
        ("::/96 (v4-compatible)", "::8.8.8.8"),
        ("64:ff9b::/96 (NAT64)", "64:ff9b::a00:1"),
        ("64:ff9b::/96 (NAT64)", "64:ff9b::808:808"),
        ("64:ff9b:1::/48 (NAT64)", "64:ff9b:1::1"),
        ("100::/64", "100::1"),
        ("2001::/32 (Teredo)", "2001:0:4136:e378::1"),
        ("2001:db8::/32", "2001:db8::1"),
        ("2002::/16 (6to4)", "2002:a00:1::1"),
        ("fc00::/7", "fd00::1"),
        ("fe80::/10", "fe80::1"),
        ("ff00::/8", "ff02::1"),
    ];

    const PUBLIC: &[&str] = &["1.1.1.1", "8.8.8.8", "198.17.255.1", "198.20.0.1", "192.0.1.1", "2606:4700:4700::1111", "2a00:1450:4001::1", "2001:4860::8888", "::ffff:8.8.8.8"];

    #[test]
    fn special_ranges_are_not_public() {
        for (range, address) in NOT_PUBLIC {
            assert!(!is_public(address.parse().unwrap()), "{address} in {range}");
        }
        for address in PUBLIC {
            assert!(is_public(address.parse().unwrap()), "{address}");
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
