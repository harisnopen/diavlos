# Relays

A relay carries encrypted bytes between two helpers when a direct link is
not possible (hotel wifi, office firewall). It never sees a message. It can
see who talks to whom, when, and how much.

## Default: n0's public relays

Out of the box, helpers use the public relays run by n0 (iroh.computer),
over HTTPS on port 443, so the demo does not die in a client's building.
Proxy settings from the environment are respected.

There is no Diavlos-hosted relay at launch. That decision is revisited at
v1.0 with real usage numbers; anyone can self-host in the meantime.

## Self-host

The relay is iroh's own, and it is small. On a box with a public address:

```sh
cargo install iroh-relay
iroh-relay --dev            # for a quick test; see `iroh-relay --help` for TLS
```

Put it behind TLS on 443 (Caddy or nginx work), then point your helpers at
it in `~/.diavlos/config.toml`:

```toml
[helper]
public_relays = false
relay_urls = ["https://relay.example.com"]
```

`public_relays = false` means nothing ever goes to n0's servers, including
address lookups.

Already on a VPN like Tailscale or WireGuard? You may not need a relay at
all: see [PRIVATE-NETWORKS.md](PRIVATE-NETWORKS.md).

## Relay access control

A private relay should serve only your nodes. iroh-relay takes an access
list of node ids (the `access` section of its config; node ids come from
`diavlos status`). With that, a stranger cannot even use your relay to
find out that your helpers exist.
