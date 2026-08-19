//! Turns a start command into the configuration EasyTier will run with.
//!
//! # Why this is built with typed setters and not by writing TOML
//!
//! The obvious implementation formats a TOML document and hands it to
//! `TomlConfigLoader::new_from_str`. It has a failure mode that cannot be
//! tested away: `Config` does not carry `deny_unknown_fields`, so a misspelled
//! key is **ignored in silence**. A typo in `disable_upnp` would leave port
//! mapping on, the engine would start fine, and nothing anywhere would say so.
//!
//! That is not hypothetical. `rpc_portal` was removed from the TOML in v2.5.0,
//! and writing it today produces no error at all: it is simply dropped.
//!
//! `ConfigLoader` exposes a typed setter for every field. A misspelling stops
//! being a silent runtime behaviour and becomes a compile error.
//!
//! # The flags are written whole, never patched
//!
//! [`flags`] starts from EasyTier's defaults and returns a **complete** struct
//! with every field Kanpachi cares about assigned explicitly, including the
//! ones whose default already matches. A default that changes upstream then
//! cannot turn on a forbidden capability without somebody writing it here.

use anyhow::Context;
use easytier::common::config::{
    gen_default_flags, process_secure_mode_cfg, ConfigLoader, Flags, NetworkIdentity, PeerConfig,
    TomlConfigLoader,
};
use easytier::proto::common::SecureModeConfig;

use crate::proto::{Common, GuestArgs, HostArgs, RendezvousArgs};

/// The flags Kanpachi runs with, stated in full.
///
/// | Flag | Value | Why |
/// |---|---|---|
/// | `disable_upnp` | `true` | **EasyTier's default is `false`**, so it maps ports on the user's router unless told not to. The router is never touched |
/// | `enable_exit_node` | `false` | never an exit node |
/// | `accept_dns` | `false` | magic DNS edits the machine's DNS and opens a loopback port. That port is the one an earlier measurement missed |
/// | `enable_udp_broadcast_relay` | `false` | captures the traffic of the user's home network with a packet-capture driver |
/// | `bind_device` | `true` | binds the outbound connector to physical interfaces. Without it, once the virtual adapter exists the engine tries to reach the seed through the virtual adapter itself |
/// | `no_tun` | `false` | there has to be an adapter, and a test that measures ports with `no_tun` on measures nothing |
/// | `private_mode` | `true` | refuses peers from other networks. Nothing about a game room needs to forward for strangers |
/// | `enable_encryption` | `true` | the default, restated so that turning it off has to be deliberate |
///
/// `enable_ipv6` stays `true`, and that is not an oversight. It governs whether
/// peers are **reached** over IPv6 (`connector/direct.rs` filters listener URLs
/// by it), not what the virtual network hands out. Turning it off would hurt
/// connectivity for users whose homes have IPv6. Kanpachi's rule about IPv6
/// being blocked lives one layer down, in the WFP gate on the virtual adapter,
/// and confusing the two would cost connectivity for nothing.
fn flags(common: &Common) -> Flags {
    let mut f = gen_default_flags();

    f.dev_name = common.dev_name.clone();
    if let Some(mtu) = common.mtu {
        f.mtu = mtu;
    }

    f.disable_upnp = true;
    f.enable_exit_node = false;
    f.accept_dns = false;
    f.enable_udp_broadcast_relay = false;
    f.bind_device = true;
    f.no_tun = false;
    f.private_mode = true;
    f.enable_encryption = true;

    f
}

/// The parts every network shares.
///
/// The three `set_*` calls at the end are the ones that matter most, and they
/// all pass empty. Each one is a capability the product forbids, and leaving
/// them unset would mean trusting a default rather than stating a decision.
fn base(common: &Common) -> anyhow::Result<TomlConfigLoader> {
    let cfg = TomlConfigLoader::default();

    cfg.set_inst_name(common.dev_name.clone());
    cfg.set_hostname(Some(common.hostname.clone()));
    cfg.set_flags(flags(common));

    let mut peers = Vec::with_capacity(common.peers.len());
    for p in &common.peers {
        // The daemon resolved and checked these. A bare name here would be
        // resolved again inside EasyTier, and that check would govern nothing.
        let uri = url::Url::parse(p).with_context(|| format!("seed address {p:?} is not a URL"))?;
        peers.push(PeerConfig {
            uri,
            peer_public_key: None,
        });
    }
    cfg.set_peers(peers);

    // The client never listens on a public port. Only the seed listens.
    cfg.set_listeners(Vec::new());
    // Publishing reachable addresses is listening in public under another name.
    cfg.set_mapped_listeners(None);
    // No subnet routing, no exit node, no proxy of local networks.
    cfg.set_exit_nodes(Vec::new());
    cfg.clear_proxy_cidrs();
    cfg.set_routes(None);
    // No SOCKS5 portal and no port forwarding, which would open something the
    // exposure module can neither see nor audit.
    cfg.set_socks5_portal(None);
    cfg.set_port_forwards(Vec::new());
    // Credentials live in memory. Pointing this at a path would put the room's
    // secrets on disk, where nothing in the product needs them.
    cfg.set_credential_file(None);

    Ok(cfg)
}

/// Turns secure mode on with a freshly generated keypair.
///
/// # Why every node needs this, and not only the guest
///
/// A credential node authenticates with a Noise handshake, and the other end
/// answers it **only if it has secure mode too**: `peer_conn.rs` takes the
/// Noise branch on `is_secure_mode_enabled() && packet_type ==
/// NoiseHandshakeMsg1`, and everything else falls through to
/// `unexpected packet type during handshake: 13`, which closes the connection.
///
/// So a host without this refuses the very guests it just issued credentials
/// to. Over the relay it merely never connects; on a hole-punched direct link
/// it is worse, because the room falls back to relaying every packet of the
/// game through the seed. Upstream's own credential tests enable it on every
/// node in the topology, admin included.
///
/// No key is passed: `process_secure_mode_cfg` generates one per start. The
/// identity being checked here is the guest's credential, not the host's, and
/// a key that survived restarts would be a secret on disk buying nothing.
fn secure() -> anyhow::Result<SecureModeConfig> {
    process_secure_mode_cfg(SecureModeConfig {
        enabled: true,
        local_private_key: None,
        local_public_key: None,
    })
    .context("generating the keypair for secure mode")
}

/// The admin node: the only one that knows the real network secret.
pub fn host(args: &HostArgs) -> anyhow::Result<TomlConfigLoader> {
    let cfg = base(&args.common)?;
    cfg.set_secure_mode(Some(secure()?));
    cfg.set_network_identity(NetworkIdentity::new(
        args.network_name.clone(),
        args.network_secret.clone(),
    ));
    cfg.set_ipv4(Some(args.ipv4.parse().with_context(|| {
        format!(
            "room address {:?} is not an address with a prefix",
            args.ipv4
        )
    })?));
    cfg.set_dhcp(false);
    Ok(cfg)
}

/// The lobby: a second network, public and disposable by design.
///
/// It is a separate network and not a mode of the room. Everyone holding the
/// invite code can derive its identity, the seed included, and the only thing
/// that happens inside is asking the host for a credential.
pub fn rendezvous(args: &RendezvousArgs) -> anyhow::Result<TomlConfigLoader> {
    let cfg = base(&args.common)?;
    // Same as the room: a guest that reaches the lobby over a direct link
    // opens it with Noise, and without this the lobby drops it and the
    // credential exchange never happens.
    cfg.set_secure_mode(Some(secure()?));
    cfg.set_network_identity(NetworkIdentity::new(
        args.network_name.clone(),
        args.network_secret.clone(),
    ));
    cfg.set_ipv4(Some(args.ipv4.parse().with_context(|| {
        format!(
            "lobby address {:?} is not an address with a prefix",
            args.ipv4
        )
    })?));
    cfg.set_dhcp(false);
    Ok(cfg)
}

/// A temporary node in the real network.
///
/// `NetworkIdentity::new_credential` takes the network **name** and no secret.
/// That is the whole reason revoking a credential closes the door: whoever came
/// in this way never held anything they could come back with.
///
/// # The address is SET here, and DHCP is off
///
/// It used to be the other way round, with a comment saying the address
/// "arrives from the host as part of admission". Half of that was true: the
/// host does decide it and does write it into the credential. What was false is
/// that anybody told this side, so `dhcp` picked its own and the two numbers
/// only matched until somebody reconnected. See [`crate::proto::GuestArgs`].
///
/// Turning DHCP off buys a second thing, and it may be the bigger one.
/// EasyTier's `check_dhcp_ip_conflict` loop calls `clear_nic_ctx` whenever its
/// chosen address stops fitting the peers it sees, which **destroys and
/// recreates the adapter**. When the host leaves a room, the only route left is
/// the public seed, which holds no address inside the room, so the loop falls
/// back to its default `10.126.126.0/24` and tears the guest's `kanpachi0`
/// down. That matches what a guest's log showed on 2026-08-08: `no hay ningún
/// adaptador llamado "kanpachi0"` with the room still open. With `dhcp` off the
/// loop is never spawned.
///
/// The lobby has always worked this way —fixed address, `dhcp` off— and never
/// had either problem.
///
/// The credential itself is an X25519 private key, and it travels in
/// `SecureModeConfig`. `process_secure_mode_cfg` derives the matching public
/// key, which is what the host recognises, and it is also what validates the
/// secret: a malformed credential fails here rather than as a connection that
/// never completes for reasons nobody can read.
pub fn guest(args: &GuestArgs) -> anyhow::Result<TomlConfigLoader> {
    let cfg = base(&args.common)?;
    cfg.set_network_identity(NetworkIdentity::new_credential(args.network_name.clone()));
    cfg.set_ipv4(Some(args.ipv4.parse().with_context(|| {
        format!(
            "guest address {:?} is not an address with a prefix",
            args.ipv4
        )
    })?));
    cfg.set_dhcp(false);
    cfg.set_secure_mode(Some(
        process_secure_mode_cfg(SecureModeConfig {
            enabled: true,
            local_private_key: Some(args.credential_secret.clone()),
            local_public_key: None,
        })
        .context("the credential is not a usable private key")?,
    ));
    Ok(cfg)
}
