//! Private network only: talk over a mesh VPN (Tailscale, Headscale,
//! NetBird, ZeroTier, Nebula, WireGuard, ...) and nothing else.
//!
//! Set `private_networks` in the config and the helper binds only to its
//! address on that network, uses no relay, dials only addresses
//! inside it, and refuses links that come from outside it. No vendor code:
//! any network that gives this machine an IP in a known range works.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use ipnet::IpNet;

/// Names for the ranges the common products hand out, so a config can say
/// `["tailscale"]` instead of a CIDR. Anything else is written as a CIDR.
const NAMED: &[(&str, &[&str])] = &[
    ("tailscale", &["100.64.0.0/10", "fd7a:115c:a1e0::/48"]),
    ("headscale", &["100.64.0.0/10", "fd7a:115c:a1e0::/48"]),
    ("netbird", &["100.64.0.0/10"]),
];

/// The ranges this helper may talk inside.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivateNetworks {
    nets: Vec<IpNet>,
}

impl PrivateNetworks {
    /// Parse config entries: CIDRs ("10.147.17.0/24") or names ("tailscale").
    pub fn parse(entries: &[String]) -> anyhow::Result<Self> {
        let mut nets = Vec::new();
        for entry in entries {
            let e = entry.trim();
            if let Some((_, cidrs)) = NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(e)) {
                for c in *cidrs {
                    nets.push(c.parse().expect("built-in range"));
                }
                continue;
            }
            let net: IpNet = e.parse().map_err(|_| {
                anyhow::anyhow!(
                    "private_networks: {e:?} is not a CIDR (like 10.147.17.0/24) or a known name ({})",
                    NAMED.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                )
            })?;
            nets.push(net.trunc());
        }
        nets.sort();
        nets.dedup();
        Ok(PrivateNetworks { nets })
    }

    /// Off when no range is set.
    pub fn is_on(&self) -> bool {
        !self.nets.is_empty()
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            v4 => v4,
        };
        self.nets.iter().any(|n| n.contains(&ip))
    }

    /// This machine's addresses on the private network, at most one per
    /// address family. Asks the OS which local address it would use to
    /// reach each range; no packet is sent.
    pub fn local_addrs(&self) -> Vec<IpAddr> {
        let mut v4 = None;
        let mut v6 = None;
        for net in &self.nets {
            let probe = probe_addr(net);
            let slot = if probe.is_ipv4() { &mut v4 } else { &mut v6 };
            if slot.is_some() {
                continue;
            }
            if let Some(local) = route_source(probe) {
                if self.contains(local) {
                    *slot = Some(local);
                }
            }
        }
        v4.into_iter().chain(v6).collect()
    }

    pub fn describe(&self) -> String {
        self.nets
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// An address inside `net` that is not the network address itself.
fn probe_addr(net: &IpNet) -> IpAddr {
    match net {
        IpNet::V4(n) => {
            let base = u32::from(n.network());
            IpAddr::V4(Ipv4Addr::from(
                base | if n.prefix_len() < 32 { 1 } else { 0 },
            ))
        }
        IpNet::V6(n) => {
            let base = u128::from(n.network());
            IpAddr::V6(Ipv6Addr::from(
                base | if n.prefix_len() < 128 { 1 } else { 0 },
            ))
        }
    }
}

/// The local address the OS picks to reach `dest`. A UDP connect only
/// sets the route; nothing goes on the wire.
fn route_source(dest: IpAddr) -> Option<IpAddr> {
    let bind: SocketAddr = match dest {
        IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect((dest, 9)).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(e: &[&str]) -> PrivateNetworks {
        PrivateNetworks::parse(&e.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn names_and_cidrs() {
        let p = nets(&["tailscale", "10.147.17.0/24"]);
        assert!(p.is_on());
        assert!(p.contains("100.101.102.103".parse().unwrap()));
        assert!(p.contains("fd7a:115c:a1e0::1".parse().unwrap()));
        assert!(p.contains("10.147.17.9".parse().unwrap()));
        assert!(!p.contains("192.168.1.10".parse().unwrap()));
        assert!(!p.contains("8.8.8.8".parse().unwrap()));
        // A v4 address seen through a dual-stack socket.
        assert!(p.contains("::ffff:100.64.0.7".parse().unwrap()));
    }

    #[test]
    fn empty_is_off() {
        assert!(!nets(&[]).is_on());
    }

    #[test]
    fn bad_entry_says_why() {
        let err = PrivateNetworks::parse(&["tailnet".into()]).unwrap_err();
        assert!(err.to_string().contains("tailscale"));
    }

    #[test]
    fn host_bits_are_dropped() {
        assert_eq!(nets(&["10.0.0.5/8"]).describe(), "10.0.0.0/8");
    }

    #[test]
    fn loopback_range_finds_loopback() {
        // Every machine has a route to 127.0.0.0/8, through itself.
        let p = nets(&["127.0.0.0/8"]);
        assert_eq!(
            p.local_addrs(),
            vec!["127.0.0.1".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn no_route_finds_nothing() {
        // TEST-NET-3: documentation only, never a local address.
        assert!(nets(&["203.0.113.0/24"]).local_addrs().is_empty());
    }
}
