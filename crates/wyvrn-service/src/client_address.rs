//! Who a request is from, behind a reverse proxy: one rule for every service
//! (security review WYV-74, WYV-86).
//!
//! `X-Forwarded-For` is believed only from a configured proxy, and then the
//! rightmost address in it that isn't one of those proxies is the client's (what
//! a client sends itself is to the left of what the proxy added, so it can't put
//! itself there). From anyone else the connection's own address is the client.
//! A limit keyed on a client goes by [`bucket`], which takes an IPv6 address by its
//! /64, as one machine has a whole /64 to move about in.

use std::net::{IpAddr, Ipv6Addr};

use axum::http::HeaderMap;

/// The Docker networks' gateways (`172.16.0.1` to `172.31.0.1`): where a proxy in
/// front of a service in a container is seen from, and nothing else on the
/// network is (another container is never a gateway).
pub fn docker_gateways() -> Vec<String> {
    (16..=31).map(|n| format!("172.{n}.0.1")).collect()
}

/// The proxies whose `X-Forwarded-For` is believed.
#[derive(Clone, Debug, Default)]
pub struct Proxies(Vec<(IpAddr, u8)>);

impl Proxies {
    /// From addresses or ranges (`10.0.0.0/8`, `fd00::/8`, `172.18.0.1`).
    pub fn parse<S: AsRef<str>>(ranges: &[S]) -> anyhow::Result<Self> {
        ranges
            .iter()
            .map(|range| parse_range(range.as_ref()).ok_or_else(|| anyhow::anyhow!("{:?} isn't an address or a range", range.as_ref())))
            .collect::<anyhow::Result<_>>()
            .map(Self)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `address` is one of these proxies.
    pub fn trusts(&self, address: IpAddr) -> bool {
        self.0.iter().any(|(net, bits)| in_range(address, *net, *bits))
    }

    /// The client a request from `peer` with `headers` is from.
    pub fn client(&self, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
        if !self.trusts(peer) {
            return peer;
        }
        headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .filter_map(|a| a.parse::<IpAddr>().ok())
            .find(|a| !self.trusts(*a))
            .unwrap_or(peer)
    }
}

/// What a per-client limit is keyed on: an IPv4 address as it is, an IPv6 one by
/// its /64 (an IPv4-mapped one as the IPv4 address).
pub fn bucket(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        },
        v4 => v4,
    }
}

fn parse_range(text: &str) -> Option<(IpAddr, u8)> {
    let (address, bits) = match text.trim().split_once('/') {
        Some((address, bits)) => (address.parse::<IpAddr>().ok()?, bits.parse::<u8>().ok()?),
        None => {
            let address = text.trim().parse::<IpAddr>().ok()?;
            (address, if address.is_ipv4() { 32 } else { 128 })
        }
    };
    let most = if address.is_ipv4() { 32 } else { 128 };
    (bits <= most).then_some((address, bits))
}

fn in_range(address: IpAddr, net: IpAddr, bits: u8) -> bool {
    let address = match (address, net) {
        (IpAddr::V6(v6), IpAddr::V4(_)) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    };
    match (address, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) => {
            let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) => {
            let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(xff: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for value in xff {
            map.append("x-forwarded-for", value.parse().unwrap());
        }
        map
    }

    #[test]
    fn forwarded_for_is_believed_only_from_a_proxy_and_read_from_the_right() {
        let proxies = Proxies::parse(&docker_gateways()).unwrap();
        let caddy: IpAddr = "172.18.0.1".parse().unwrap();
        // Through the proxy: the address it added, not what the client claimed before it.
        assert_eq!(proxies.client(caddy, &headers(&["10.9.9.9, 203.0.113.5"])), "203.0.113.5".parse::<IpAddr>().unwrap());
        // Two proxies deep: the first one that isn't a proxy, from the right.
        assert_eq!(proxies.client(caddy, &headers(&["198.51.100.7, 172.19.0.1"])), "198.51.100.7".parse::<IpAddr>().unwrap());
        // Another container (not a gateway) can't pass as the proxy.
        let other: IpAddr = "172.18.0.5".parse().unwrap();
        assert_eq!(proxies.client(other, &headers(&["203.0.113.5"])), other);
        // A proxy with nothing forwarded is the client.
        assert_eq!(proxies.client(caddy, &headers(&[])), caddy);
        // With no proxies, nobody's header is believed.
        assert_eq!(Proxies::default().client(caddy, &headers(&["203.0.113.5"])), caddy);
    }

    #[test]
    fn ipv6_clients_are_limited_by_their_64() {
        let a = bucket("2001:db8:1:2:aaaa::1".parse().unwrap());
        let b = bucket("2001:db8:1:2:bbbb::9".parse().unwrap());
        let c = bucket("2001:db8:1:3::1".parse().unwrap());
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(bucket("::ffff:203.0.113.5".parse().unwrap()), "203.0.113.5".parse::<IpAddr>().unwrap());
        assert!(Proxies::parse(&["not an address"]).is_err());
    }
}
