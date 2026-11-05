use crate::middleware::proxy_matches;
use axum::extract::connect_info::ConnectInfo;
use axum::extract::Request;
use std::net::{IpAddr, SocketAddr};

/// Hashable form of a client address. IPv4 stays exact; IPv6 collapses to its
/// /64 prefix so one client cannot rotate through its subnet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum IpKey {
    V4(u32),
    V6(u64),
}

impl From<IpAddr> for IpKey {
    fn from(address: IpAddr) -> Self {
        match address {
            IpAddr::V4(address) => Self::V4(u32::from(address)),
            IpAddr::V6(address) => Self::V6((u128::from(address) >> 64) as u64),
        }
    }
}

fn peer(request: &Request) -> Option<SocketAddr> {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0)
        .or_else(|| request.extensions().get::<SocketAddr>().copied())
}

fn peer_is_trusted(peer: SocketAddr, trusted: &[String]) -> bool {
    trusted
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .any(|entry| proxy_matches(peer, entry))
}

/// Resolve the client address.
///
/// Without a peer (in-process calls, tests) there is nothing to limit. A peer
/// outside `trusted` is the client. For a trusted peer the rightmost untrusted
/// `X-Forwarded-For` entry wins, so a header a client prepends cannot bypass
/// the source bucket.
pub(crate) fn client_ip(request: &Request, trusted: &[String]) -> Option<IpAddr> {
    let peer = peer(request)?;
    if !peer_is_trusted(peer, trusted) {
        return Some(peer.ip());
    }
    let forwarded = request.headers().get("x-forwarded-for")?.to_str().ok()?;
    for entry in forwarded.rsplit(',').map(str::trim) {
        let Ok(address) = entry.parse::<IpAddr>() else {
            return Some(peer.ip());
        };
        if !peer_is_trusted(SocketAddr::new(address, 0), trusted) {
            return Some(address);
        }
    }
    Some(peer.ip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::header::HeaderName;

    fn client(peer: IpAddr, forwarded: Option<&str>, trusted: &[String]) -> Option<IpAddr> {
        let mut request = Request::builder().uri("/").body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer, 1234)));
        if let Some(value) = forwarded {
            request.headers_mut().insert(
                HeaderName::from_static("x-forwarded-for"),
                value.parse().unwrap(),
            );
        }
        client_ip(&request, trusted)
    }

    #[test]
    fn untrusted_peer_ignores_forwarded_for() {
        let peer: IpAddr = "203.0.113.9".parse().unwrap();
        assert_eq!(client(peer, Some("198.51.100.1"), &[]), Some(peer));
    }

    #[test]
    fn trusted_peer_uses_the_rightmost_untrusted_address() {
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let trusted = vec!["127.0.0.1".to_string(), "10.0.0.0/8".to_string()];
        assert_eq!(
            client(peer, Some("1.2.3.4, 10.0.0.5, 127.0.0.1"), &trusted),
            Some("1.2.3.4".parse().unwrap())
        );
        assert_eq!(
            client(peer, Some("1.2.3.4, 127.0.0.1"), &trusted),
            Some("1.2.3.4".parse().unwrap())
        );
    }

    #[test]
    fn a_trusted_chain_without_a_client_falls_back_to_the_peer() {
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        let trusted = vec!["127.0.0.1".to_string(), "10.0.0.0/8".to_string()];
        assert_eq!(
            client(peer, Some("10.0.0.5, 127.0.0.1"), &trusted),
            Some(peer)
        );
        assert_eq!(client(peer, Some("garbage"), &trusted), Some(peer));
    }

    #[test]
    fn without_a_peer_there_is_no_client() {
        let request = Request::builder().uri("/").body(Body::empty()).unwrap();
        assert_eq!(client_ip(&request, &[]), None);
    }

    #[test]
    fn ipv6_keys_on_the_64_bit_prefix() {
        let a: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2::ffff".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(IpKey::from(a), IpKey::from(b));
        assert_ne!(IpKey::from(a), IpKey::from(c));
    }
}
