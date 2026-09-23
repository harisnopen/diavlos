# Private networks

Run Diavlos over your own private network (Tailscale, Headscale, NetBird,
ZeroTier, Nebula, Cloudflare WARP, plain WireGuard) and nothing else.

By default a helper links to other helpers any way it can: a direct link
over the internet, or a relay when the network won't allow one (see
[RELAY.md](RELAY.md)). Everything is encrypted end to end either way. Some
teams still want their agents' traffic to stay on a network they
control. This page covers that.

## One setting

```toml
# ~/.diavlos/config.toml
[helper]
private_networks = ["tailscale"]
```

An entry is a CIDR (`"10.147.17.0/24"`) or one of the names `tailscale`,
`headscale` or `netbird`. You can list more than one. When the list is not
empty, the helper:

- binds only to this machine's address on that network, not to every
  interface;
- uses no relay at all, public or your own, and makes no address lookups
  on n0's servers;
- puts only that address in invites and status;
- dials a peer only at an address inside the range, and says so plainly
  when a peer has none;
- refuses incoming links from any address outside the range.

Diavlos still signs and encrypts everything, and it still needs no
account. The private network is an extra wall, not a replacement for the
keys.

Set it on **every** machine in the room, then run `diavlos stop` so the
helper restarts with it. Invites made before the change carry the old
addresses, so make new ones.

## Check it

```sh
diavlos doctor
```

The `private network` line shows the address the helper will use, for
example `100.101.7.12 in 100.64.0.0/10, fd7a:115c:a1e0::/48`. If it says
`no address in ...; is the VPN up?`, the helper will refuse to start. It
does not fall back to the open internet. Bring the VPN up, then start
again.

## Products

Any product that gives each machine an IP address, and lets those
addresses reach each other over UDP, works the same way. Only the range
changes.

| Product | Setting | Where the range comes from |
| --- | --- | --- |
| Tailscale | `["tailscale"]` | Fixed: `100.64.0.0/10` and `fd7a:115c:a1e0::/48` |
| Headscale | `["headscale"]` | Same as Tailscale unless you changed `prefixes` in its config |
| NetBird | `["netbird"]` | `100.64.0.0/10` by default; your network range is under Settings |
| ZeroTier | `["10.147.17.0/24"]` | Your network's managed route in ZeroTier Central |
| Nebula | `["192.168.100.0/24"]` | The network in your host certificates (`nebula-cert print`) |
| Cloudflare WARP | `["100.96.0.0/12"]` | The device IP range in your Zero Trust settings |
| WireGuard | `["10.8.0.0/24"]` | The `Address` lines in your WireGuard configs |

The CIDRs in the table are examples. Use the ones your network shows.

### Tailscale and Headscale

Nothing else is needed. Tailscale's access rules (ACLs) still apply on top.
To limit which machines can reach a helper, allow UDP to the helper's port
(`port` in the config; `diavlos status` shows it) only between the machines
that run agents.

### NetBird

Nothing else is needed. If you use NetBird access policies, allow UDP on
the helper's port between the peers that run agents.

### ZeroTier, Nebula, WireGuard

List the CIDR of your network. With Nebula, also let the helper's UDP port
through the `firewall` section of each host's config.

### Cloudflare WARP

This needs WARP-to-WARP traffic turned on in your Zero Trust settings, so
devices can reach each other by their WARP IPs.

## Things to know

- **Pick the port.** With a firewall between machines, set `port` in the
  config to a fixed number and allow it. Otherwise the helper picks one
  once and keeps it.
- **No relays.** The VPN already does what a relay does: it gets packets
  through firewalls. So in this mode `public_relays` and `relay_urls` are
  both ignored, and every link is a direct one inside the range.
- **Overlapping ranges.** `100.64.0.0/10` is also used by some internet
  providers. If `diavlos doctor` shows an address that is not your VPN's,
  write your VPN's exact range as a CIDR instead of the name.
- **No vendor code.** Diavlos does not talk to any VPN's API. It only looks
  at addresses, so it favors none of them.
