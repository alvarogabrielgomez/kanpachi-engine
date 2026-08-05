//! The two networks, their event pumps, and the in-process calls.
//!
//! # Why two instances and not one with a mode
//!
//! A host is in two networks at the same time: the room, and the lobby that
//! everyone holding the invite code can derive. They have different identities
//! and different trust models, so they are two [`NetworkInstance`] values with
//! two configurations. `leave_rendezvous` closes only the lobby.
//!
//! Collapsing them into one would break the case it exists for: a guest has to
//! drop the lobby and stay in the room, and a single "leave" would throw them
//! out of the room they just entered.
//!
//! # The portal that is never built
//!
//! Everything the daemon asks for is reached through
//! `NetworkInstance::get_api_service()`, which hands back the very same
//! `InstanceRpcService` the official binary publishes on a TCP port. The
//! difference is one line that does not exist here: `ApiRpcServer::new` is
//! constructed in exactly one place in EasyTier's tree, inside its
//! command-line binary. Nothing on the library path names it. The engine gets
//! the full surface and opens no socket.

use std::time::Duration;

use anyhow::{Context, anyhow};
use easytier::common::global_ctx::GlobalCtxEvent;
use easytier::common::config::ConfigFileControl;
use easytier::launcher::NetworkInstance;
use easytier::proto::api::instance::{
    GenerateCredentialRequest, ListCredentialsRequest, ListRouteRequest, RevokeCredentialRequest,
    ShowNodeInfoRequest,
};
use easytier::proto::rpc_types::controller::BaseController;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::proto::{
    CredentialOut, CredentialSummary, DiagnosticsOut, Event, EventKind, GuestArgs, HostArgs,
    IssueArgs, Outgoing, PeerOut, RendezvousArgs, RevokeArgs,
};
use crate::config;

/// Which of the two networks a call is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slot {
    Room,
    Lobby,
}

pub struct Engine {
    room: Option<NetworkInstance>,
    lobby: Option<NetworkInstance>,
    /// What the daemon asked for when the room started. Reported back in the
    /// diagnosis; see [`Engine::diagnostics`].
    room_mtu: u32,
    out: mpsc::UnboundedSender<Outgoing>,
}

/// EasyTier's own default, restated so the diagnosis never reports a zero when
/// the daemon did not name one.
const DEFAULT_MTU: u32 = 1380;

impl Engine {
    pub fn new(out: mpsc::UnboundedSender<Outgoing>) -> Self {
        Engine { room: None, lobby: None, room_mtu: DEFAULT_MTU, out }
    }

    pub fn host(&mut self, args: &HostArgs) -> anyhow::Result<()> {
        self.room_mtu = args.common.mtu.unwrap_or(DEFAULT_MTU);
        self.start(Slot::Room, config::host(args)?)
    }

    pub fn join(&mut self, args: &GuestArgs) -> anyhow::Result<()> {
        self.room_mtu = args.common.mtu.unwrap_or(DEFAULT_MTU);
        self.start(Slot::Room, config::guest(args)?)
    }

    /// Entering the lobby REPLACES the previous one.
    ///
    /// That is what makes renewing the invite code work: the lobby's name comes
    /// from the invite id, so a new code is a new lobby, and staying in the old
    /// one would produce a code nobody can enter through.
    pub fn join_rendezvous(&mut self, args: &RendezvousArgs) -> anyhow::Result<()> {
        self.lobby = None;
        self.start(Slot::Lobby, config::rendezvous(args)?)
    }

    /// Leaves ONLY the lobby, and says nothing when there is no lobby.
    pub fn leave_rendezvous(&mut self) {
        self.lobby = None;
    }

    /// Leaves everything. Idempotent, because the daemon calls it on error
    /// paths that can run before anything ever started.
    pub fn leave(&mut self) {
        self.room = None;
        self.lobby = None;
    }

    fn start(
        &mut self,
        slot: Slot,
        cfg: easytier::common::config::TomlConfigLoader,
    ) -> anyhow::Result<()> {
        let mut instance = NetworkInstance::new(cfg, ConfigFileControl::STATIC_CONFIG);

        // `start` is synchronous: it spawns its own thread with its own Tokio
        // runtime. What it returns is the subscriber the official path throws
        // away, and keeping it is the reason this engine can push events at
        // all.
        let events = instance.start().context("starting the network instance")?;

        match slot {
            Slot::Room => self.room = Some(instance),
            Slot::Lobby => self.lobby = Some(instance),
        }

        // Only the room's events reach the daemon. The lobby is a waiting area
        // that lasts as long as a credential exchange, and reporting its peers
        // as room members would put strangers in the member list, which is
        // what the firewall opens ports for.
        if slot == Slot::Room {
            tokio::spawn(pump(events, self.out.clone()));
        }
        Ok(())
    }

    fn room(&self) -> anyhow::Result<&NetworkInstance> {
        self.room.as_ref().ok_or_else(|| anyhow!("there is no room running"))
    }

    /// Issues a credential for one member.
    ///
    /// **`reusable` is false, explicitly.** EasyTier's field defaults to true,
    /// which lets several peers come in on one credential. Kanpachi issues one
    /// per member precisely so that revoking removes one person, and a shared
    /// credential would make a kick either useless or collective.
    ///
    /// `allow_relay` is false as well: nothing about a game room asks a
    /// player's machine to carry other people's traffic.
    pub async fn issue_credential(&self, args: &IssueArgs) -> anyhow::Result<CredentialOut> {
        if args.ttl_seconds <= 0 {
            return Err(anyhow!("the credential's lifetime has to be more than zero seconds"));
        }
        let api = self.api()?;
        let res = api
            .get_credential_manage_service()
            .generate_credential(
                BaseController::default(),
                GenerateCredentialRequest {
                    groups: Vec::new(),
                    allow_relay: false,
                    allowed_proxy_cidrs: Vec::new(),
                    ttl_seconds: args.ttl_seconds,
                    credential_id: None,
                    instance: None,
                    reusable: Some(false),
                },
            )
            .await?;
        Ok(CredentialOut {
            credential_id: res.credential_id,
            credential_secret: res.credential_secret,
        })
    }

    pub async fn revoke_credential(&self, args: &RevokeArgs) -> anyhow::Result<()> {
        let api = self.api()?;
        let res = api
            .get_credential_manage_service()
            .revoke_credential(
                BaseController::default(),
                RevokeCredentialRequest {
                    credential_id: args.credential_id.clone(),
                    instance: None,
                },
            )
            .await?;
        if !res.success {
            return Err(anyhow!("no credential with id {:?}", args.credential_id));
        }
        Ok(())
    }

    /// Lists credentials WITHOUT their secrets, and that is deliberate.
    ///
    /// EasyTier's `CredentialInfo` carries no secret either. The daemon needs
    /// ids to revoke by; handing back secrets would put every member's key on
    /// the wire on every screen refresh.
    pub async fn list_credentials(&self) -> anyhow::Result<Vec<CredentialSummary>> {
        let api = self.api()?;
        let res = api
            .get_credential_manage_service()
            .list_credentials(BaseController::default(), ListCredentialsRequest { instance: None })
            .await?;
        Ok(res
            .credentials
            .into_iter()
            .map(|c| CredentialSummary {
                credential_id: c.credential_id,
                expiry_unix: c.expiry_unix,
            })
            .collect())
    }

    /// Who is present, from the ROUTE table and not from the peer list.
    ///
    /// `PeerInfo` carries a numeric peer id and its connections, with no
    /// address and no name. What the member list needs, an address and
    /// something a human recognises, lives in `Route`.
    ///
    /// `cost` is how the path is classified: one hop is a direct tunnel,
    /// anything more goes through the seed. That distinction is worth
    /// reporting because a relayed path is slower, and saying so stops the
    /// player from blaming the game for latency that belongs to the network.
    pub async fn peers(&self) -> anyhow::Result<Vec<PeerOut>> {
        let api = self.api()?;
        let svc = api.get_peer_manage_service();

        let me = svc
            .show_node_info(BaseController::default(), ShowNodeInfoRequest { instance: None })
            .await?
            .node_info
            .ok_or_else(|| anyhow!("the engine does not know its own node yet"))?;

        let mut out = vec![PeerOut {
            virtual_ip: me.ipv4_addr.clone(),
            hostname: me.hostname.clone(),
            path: "self",
            rtt_ms: 0,
        }];

        let routes = svc
            .list_route(BaseController::default(), ListRouteRequest { instance: None })
            .await?
            .routes;

        for r in routes {
            let ip = r.ipv4_addr.map(|a| format!("{a}")).unwrap_or_default();
            out.push(PeerOut {
                virtual_ip: ip,
                hostname: r.hostname,
                path: if r.cost <= 1 { "direct" } else { "relay" },
                rtt_ms: r.path_latency,
            });
        }
        Ok(out)
    }

    /// The diagnosis, which turns "it does not connect" into a sentence.
    pub async fn diagnostics(&self) -> anyhow::Result<DiagnosticsOut> {
        let api = self.api()?;
        let me = api
            .get_peer_manage_service()
            .show_node_info(BaseController::default(), ShowNodeInfoRequest { instance: None })
            .await?
            .node_info
            .ok_or_else(|| anyhow!("the engine does not know its own node yet"))?;

        let stun = me.stun_info.unwrap_or_default();
        let nat = easytier::proto::common::NatType::try_from(stun.udp_nat_type)
            .unwrap_or(easytier::proto::common::NatType::Unknown);

        Ok(DiagnosticsOut {
            nat_kind: format!("{nat:?}"),
            // A symmetric UDP firewall is the case where UDP leaves and nothing
            // comes back, which is what "UDP blocked" means to a player.
            udp_blocked: nat == easytier::proto::common::NatType::SymUdpFirewall,
            public_ips: stun.public_ip,
            virtual_ip: me.ipv4_addr,
            // The MTU the room was started with, remembered here rather than
            // read back. It is the configured value and NOT a measurement of
            // the path: probing that means sending from the machine and reading
            // the reply, which is the daemon's job and not the engine's.
            mtu: self.room_mtu,
        })
    }

    fn api(&self) -> anyhow::Result<std::sync::Arc<dyn easytier::rpc_service::InstanceRpcService>> {
        self.room()?
            .get_api_service()
            .ok_or_else(|| anyhow!("the room is not running yet"))
    }
}

/// Translates EasyTier's event bus into the four the daemon understands.
///
/// # What is dropped, and why the silence is a decision
///
/// EasyTier emits twenty-four kinds. Four of them announce capabilities this
/// product forbids: `ListenerAdded`, `PortForwardAdded`, `VpnPortalStarted` and
/// `UdpBroadcastRelayStartResult`. They are dropped without a trace, on
/// purpose. The question they would answer, "did the configuration really turn
/// that off", is answered earlier and harder by the socket invariant test,
/// which starts the engine with the real configuration and the TUN device up
/// and fails on any listening socket.
///
/// What that costs is detection on the user's machine for something that passes
/// in CI and breaks in the field. Written down here so it is a known cost and
/// not a discovery.
async fn pump(
    mut events: tokio::sync::broadcast::Receiver<GlobalCtxEvent>,
    out: mpsc::UnboundedSender<Outgoing>,
) {
    loop {
        let ev = match events.recv().await {
            Ok(ev) => ev,
            // The bus is a broadcast channel with a bounded buffer. Falling
            // behind is not an error and must not end the pump: dropping the
            // pump would leave the daemon with a room that never reports a
            // change again. A burst of joins is exactly when this happens.
            Err(RecvError::Lagged(n)) => {
                let _ = out.send(Outgoing::Event(Event::new(
                    EventKind::PeersChanged,
                    format!("{n} engine events were dropped, the state was re-read"),
                )));
                continue;
            }
            Err(RecvError::Closed) => return,
        };

        let translated = match ev {
            GlobalCtxEvent::TunDeviceReady(dev) => {
                Some(Event::new(EventKind::Connected, format!("adapter {dev} is up")))
            }
            GlobalCtxEvent::TunDeviceError(e) => Some(Event::new(
                EventKind::Disconnected,
                format!("the virtual adapter failed: {e}"),
            )),

            GlobalCtxEvent::PeerAdded(_) => {
                Some(Event::new(EventKind::PeersChanged, "somebody joined"))
            }
            GlobalCtxEvent::PeerRemoved(_) => {
                Some(Event::new(EventKind::PeersChanged, "somebody left"))
            }
            GlobalCtxEvent::PeerConnAdded(_) | GlobalCtxEvent::PeerConnRemoved(_) => {
                Some(Event::new(EventKind::PeersChanged, "a connection to a member changed"))
            }

            GlobalCtxEvent::ConnectError(dst, _, err) => {
                Some(Event::new(EventKind::Degraded, format!("could not reach {dst}: {err}")))
            }
            GlobalCtxEvent::ConnectionError(dst, _, err) => {
                Some(Event::new(EventKind::Degraded, format!("connection to {dst} failed: {err}")))
            }
            GlobalCtxEvent::DhcpIpv4Conflicted(addr) => Some(Event::new(
                EventKind::Degraded,
                format!("the address {addr:?} is already taken inside the room"),
            )),
            GlobalCtxEvent::DhcpIpv4Changed(_, new) => Some(Event::new(
                EventKind::PeersChanged,
                format!("this machine's address in the room is now {new:?}"),
            )),

            // Revoking a credential is how a kick works, so the member list
            // changed even though no peer event fired.
            GlobalCtxEvent::CredentialChanged => {
                Some(Event::new(EventKind::PeersChanged, "the credentials changed"))
            }

            _ => None,
        };

        if let Some(e) = translated {
            // A closed channel means the writer task is gone, which means the
            // process is on its way out. Nothing useful is left to do.
            if out.send(Outgoing::Event(e)).is_err() {
                return;
            }
        }
    }
}

/// A short grace period so that a `leave` immediately followed by process exit
/// still lets the virtual adapter come down cleanly.
///
/// Dropping a `NetworkInstance` signals its thread to stop; it does not wait
/// for it. Exiting the process in the same breath can leave the adapter in
/// place until Windows tears it down with the process, which the user sees as
/// a network card that lingers.
pub const SHUTDOWN_GRACE: Duration = Duration::from_millis(300);
