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

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, Context};
use easytier::common::config::ConfigFileControl;
use easytier::common::global_ctx::GlobalCtxEvent;
use easytier::launcher::NetworkInstance;
use easytier::proto::api::instance::{
    list_peer_route_pair, GenerateCredentialRequest, ListCredentialsRequest, ListPeerRequest,
    ListRouteRequest, PeerRoutePair, RenewCredentialRequest, RevokeCredentialRequest, Route,
    ShowNodeInfoRequest,
};
use easytier::proto::peer_rpc::{GetGlobalPeerMapRequest, PeerCenterRpc, PeerInfoForGlobalMap};
use easytier::proto::rpc_types::controller::BaseController;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;

use crate::config;
use crate::proto::{
    CredentialOut, CredentialSummary, DiagnosticsOut, Event, EventKind, GuestArgs, HostArgs,
    IssueArgs, Outgoing, PeerOut, RendezvousArgs, RenewArgs, RevokeArgs,
};

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
        Engine {
            room: None,
            lobby: None,
            room_mtu: DEFAULT_MTU,
            out,
        }
    }

    pub async fn host(&mut self, args: &HostArgs) -> anyhow::Result<()> {
        crate::log::init_once(args.common.log_dir.as_deref());
        self.room_mtu = args.common.mtu.unwrap_or(DEFAULT_MTU);
        self.start(Slot::Room, config::host(args)?).await
    }

    pub async fn join(&mut self, args: &GuestArgs) -> anyhow::Result<()> {
        crate::log::init_once(args.common.log_dir.as_deref());
        self.room_mtu = args.common.mtu.unwrap_or(DEFAULT_MTU);
        self.start(Slot::Room, config::guest(args)?).await
    }

    /// Entering the lobby REPLACES the previous one.
    ///
    /// That is what makes renewing the invite code work: the lobby's name comes
    /// from the invite id, so a new code is a new lobby, and staying in the old
    /// one would produce a code nobody can enter through.
    ///
    /// The replacing itself is [`Engine::start`]'s job now, for every slot. It
    /// used to be one line here and nowhere else, which is exactly why the room
    /// went without it.
    pub async fn join_rendezvous(&mut self, args: &RendezvousArgs) -> anyhow::Result<()> {
        // El invitado entra al VESTÍBULO primero, así que para él esta es la
        // primera orden del proceso y la única oportunidad de encender el log
        // antes de que empiece lo interesante.
        crate::log::init_once(args.common.log_dir.as_deref());
        self.start(Slot::Lobby, config::rendezvous(args)?).await
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

    /// Starts a network instance in a slot, REPLACING whatever was there.
    ///
    /// # Why the old one is dropped BEFORE the new one is built
    ///
    /// Because both want the same virtual adapter, named by the daemon. This
    /// used to build and start the new instance first and only drop the old one
    /// when assigning the slot, so for a moment two instances fought over
    /// `kanpachi0`.
    ///
    /// What that looked like, measured on 2026-08-08: creating a room right
    /// after another one, the adapter kept the PREVIOUS room's address. The
    /// daemon waited 30 s for the new one and reported `el adaptador
    /// "kanpachi0" no tomó la dirección 10.99.113.1 en 30s (el adaptador existe
    /// con 10.99.175.1)`.
    ///
    /// And dropping alone is not enough: it only signals the instance's thread
    /// to stop, it does not wait. Hence the grace, which is the same one the
    /// process already waits before exiting, and for the same reason.
    ///
    /// The grace is paid ONLY when there was something to replace. A first
    /// start owes nobody a wait.
    ///
    /// # What this gives up, on purpose
    ///
    /// If `start` fails, the previous room is gone rather than left running.
    /// That is a real loss and it is the better trade: the caller's error path
    /// tears the room down anyway, and two engines on one adapter is a room
    /// that connects at random, which is far worse to live with than one that
    /// failed cleanly.
    async fn start(
        &mut self,
        slot: Slot,
        cfg: easytier::common::config::TomlConfigLoader,
    ) -> anyhow::Result<()> {
        let previa = match slot {
            Slot::Room => self.room.take(),
            Slot::Lobby => self.lobby.take(),
        };
        if previa.is_some() {
            drop(previa);
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        }

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

        // The room reports everything. The lobby reports ONLY what happened to
        // its adapter.
        //
        // # Why the lobby is filtered at all
        //
        // Because it is a waiting area that anyone holding the invite code can
        // reach, and reporting its peers as room members would put strangers in
        // the list the firewall opens ports for.
        //
        // # Why it is no longer silent, which cost two days
        //
        // The lobby used to report nothing, and `TunDeviceError` is the event
        // carrying the reason a virtual adapter failed. For a GUEST the lobby is
        // the first command of the process, so the one event that says why was
        // thrown away in the only case where it mattered.
        //
        // Measured on 2026-08-11 on a guest's machine that could not join. The
        // engine had written the answer and dropped it:
        //
        //     TunDeviceError("rust tun error Failed to create adapter")
        //
        // What the daemon saw instead was thirty seconds of nothing followed by
        // a timeout about an address, which sends anyone to look at addressing,
        // the one thing that was right.
        //
        // An adapter is not a peer: saying that ours failed leaks nothing about
        // who else is in the lobby.
        let solo_adaptador = slot == Slot::Lobby;
        // El api va SOLO en la sala, y es lo que le permite al bombeo esperar a
        // que la ruta de quien entra lleve dirección antes de anunciarlo. Ver
        // [`esperar_direccion`]. El vestíbulo no lo necesita: no reporta pares.
        let api = if solo_adaptador {
            None
        } else {
            self.api().ok()
        };
        tokio::spawn(pump(events, self.out.clone(), solo_adaptador, api));
        Ok(())
    }

    fn room(&self) -> anyhow::Result<&NetworkInstance> {
        self.room
            .as_ref()
            .ok_or_else(|| anyhow!("there is no room running"))
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
            return Err(anyhow!(
                "the credential's lifetime has to be more than zero seconds"
            ));
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

    /// Pushes a credential's expiry out, keeping its keypair.
    ///
    /// # Why this is not "issue again with the same id"
    ///
    /// Because that does nothing. `generate_credential` given an id that
    /// already exists returns the stored secret and leaves the expiry where it
    /// was, so a daemon renewing that way would watch every member drop on
    /// schedule while its own calls reported success.
    ///
    /// # Why an unknown id is an error here and a flag on the wire
    ///
    /// Same shape as revoking. The RPC answers `success: false` because a
    /// caller renewing on a timer legitimately races revocation and expiry, and
    /// this turns it into an error so the daemon can tell it apart from the
    /// engine being unreachable, which is the case where retrying makes sense.
    pub async fn renew_credential(&self, args: &RenewArgs) -> anyhow::Result<i64> {
        if args.ttl_seconds <= 0 {
            return Err(anyhow!(
                "the credential's lifetime has to be more than zero seconds"
            ));
        }
        let api = self.api()?;
        let res = api
            .get_credential_manage_service()
            .renew_credential(
                BaseController::default(),
                RenewCredentialRequest {
                    credential_id: args.credential_id.clone(),
                    ttl_seconds: args.ttl_seconds,
                    instance: None,
                },
            )
            .await?;
        if !res.success {
            return Err(anyhow!("no credential with id {:?}", args.credential_id));
        }
        Ok(res.expiry_unix)
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
            .list_credentials(
                BaseController::default(),
                ListCredentialsRequest { instance: None },
            )
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
            .show_node_info(
                BaseController::default(),
                ShowNodeInfoRequest { instance: None },
            )
            .await?
            .node_info
            .ok_or_else(|| anyhow!("the engine does not know its own node yet"))?;

        let mut out = vec![PeerOut {
            virtual_ip: bare_addr(&me.ipv4_addr),
            hostname: me.hostname.clone(),
            path: "self",
            // Measuring the round trip to oneself is not a question.
            rtt_ms: None,
        }];

        let routes = svc
            .list_route(
                BaseController::default(),
                ListRouteRequest { instance: None },
            )
            .await?
            .routes;

        let peers = svc
            .list_peer(
                BaseController::default(),
                ListPeerRequest { instance: None },
            )
            .await?
            .peer_infos;
        // Every node keeps its own copy of this map in RAM and refreshes it
        // every fifteen seconds, so this call goes nowhere near the network.
        // See [`measured_rtt_ms`] for what is read out of it.
        let global = api
            .get_peer_center_service()
            .get_global_peer_map(
                BaseController::default(),
                GetGlobalPeerMapRequest { digest: 0 },
            )
            .await?
            .global_peer_map;
        // Built from the UNFILTERED routes on purpose: the seed is dropped from
        // the member list below and is still needed here, because it is the
        // middle hop of every relayed path.
        let pairs = list_peer_route_pair(peers, routes.clone());

        for r in routes {
            // A node with no address in the room is not a member of it.
            //
            // The public seed lands here: it relays for the room and does not
            // live in its address space, so it comes back with no `ipv4_addr`.
            // Reporting it put a nameless member on everyone's screen, and
            // handed the daemon a member to key firewall rules on that has no
            // address to key them on.
            let Some(addr) = r.ipv4_addr else { continue };
            let rtt_ms = measured_rtt_ms(&r, &pairs, &global);
            out.push(PeerOut {
                virtual_ip: bare_addr(&format!("{addr}")),
                hostname: r.hostname,
                path: if r.cost <= 1 { "direct" } else { "relay" },
                rtt_ms,
            });
        }
        Ok(out)
    }

    /// The diagnosis, which turns "it does not connect" into a sentence.
    pub async fn diagnostics(&self) -> anyhow::Result<DiagnosticsOut> {
        let api = self.api()?;
        let me = api
            .get_peer_manage_service()
            .show_node_info(
                BaseController::default(),
                ShowNodeInfoRequest { instance: None },
            )
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
            engine_build: crate::build_id::BUILD.to_string(),
            engine_lib: crate::build_id::LIB.to_string(),
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
/// `solo_adaptador` keeps everything except the adapter's own fate off the wire.
/// It is what the lobby runs with; see the call site in [`Engine::start`].
async fn pump(
    mut events: tokio::sync::broadcast::Receiver<GlobalCtxEvent>,
    out: mpsc::UnboundedSender<Outgoing>,
    solo_adaptador: bool,
    api: Option<std::sync::Arc<dyn easytier::rpc_service::InstanceRpcService>>,
) {
    loop {
        let ev = match events.recv().await {
            Ok(ev) => ev,
            // The bus is a broadcast channel with a bounded buffer. Falling
            // behind is not an error and must not end the pump: dropping the
            // pump would leave the daemon with a room that never reports a
            // change again. A burst of joins is exactly when this happens.
            //
            // Lagging is about PEERS, so the lobby says nothing: its own
            // re-read would be a room event about a network that has no
            // members as far as the daemon is concerned.
            Err(RecvError::Lagged(n)) if solo_adaptador => {
                let _ = n;
                continue;
            }
            Err(RecvError::Lagged(n)) => {
                let _ = out.send(Outgoing::Event(Event::new(
                    EventKind::PeersChanged,
                    format!("{n} engine events were dropped, the state was re-read"),
                )));
                continue;
            }
            Err(RecvError::Closed) => return,
        };

        // The lobby's whole vocabulary is ONE event: its adapter failed.
        //
        // Not `TunDeviceReady`, and that exclusion carries weight. Ready
        // translates to `Connected`, and the daemon's supervisor treats
        // Connected as "the tunnel is up": for a guest still exchanging a
        // credential in the lobby that would fire the room's whole
        // connected-path — rebinding, rule application, announcements — on a
        // network that is not the room. The daemon already detects lobby
        // readiness by polling the adapter's address, so Ready adds nothing
        // and can mislead.
        //
        // The error, in contrast, is the one fact the daemon cannot get any
        // other way, and dropping it is what turned a named driver problem
        // into a thirty-second silence. See the call site.
        if solo_adaptador && !matches!(ev, GlobalCtxEvent::TunDeviceError(_)) {
            continue;
        }

        let translated = match ev {
            GlobalCtxEvent::TunDeviceReady(dev) => Some(Event::new(
                EventKind::Connected,
                format!("adapter {dev} is up"),
            )),
            GlobalCtxEvent::TunDeviceError(e) => Some(Event::new(
                EventKind::Disconnected,
                format!("the virtual adapter failed: {e}"),
            )),

            GlobalCtxEvent::PeerAdded(id) => {
                Some(esperar_direccion(api.as_ref(), id, "somebody joined").await)
            }
            GlobalCtxEvent::PeerRemoved(_) => {
                Some(Event::new(EventKind::PeersChanged, "somebody left"))
            }
            GlobalCtxEvent::PeerConnAdded(info) => Some(
                esperar_direccion(
                    api.as_ref(),
                    info.peer_id,
                    "a connection to a member came up",
                )
                .await,
            ),
            GlobalCtxEvent::PeerConnRemoved(_) => Some(Event::new(
                EventKind::PeersChanged,
                "a connection to a member changed",
            )),

            GlobalCtxEvent::ConnectError(dst, _, err) => Some(Event::new(
                EventKind::Degraded,
                format!("could not reach {dst}: {err}"),
            )),
            GlobalCtxEvent::ConnectionError(dst, _, err) => Some(Event::new(
                EventKind::Degraded,
                format!("connection to {dst} failed: {err}"),
            )),
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
            GlobalCtxEvent::CredentialChanged => Some(Event::new(
                EventKind::PeersChanged,
                "the credentials changed",
            )),

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

/// The round trip to one member, measured, or nothing.
///
/// # Where each number comes from
///
/// One hop is a tunnel this machine holds, so the answer is in its own
/// connection stats: `PeerRoutePair::get_latency_ms` reads `stats.latency_us`
/// off the default connection, or the lowest of them. Nothing else is
/// consulted for that hop, and that is deliberate: a route with a cost of one
/// and no live connection is EVIDENCE, the exact case the daemon's dial
/// diagnosis reads, and filling it in from elsewhere would blunt it.
///
/// Two hops is the room's relay: this machine to the seed, and the seed to the
/// member. The first half is a local measurement again. The second half is one
/// the SEED took on its own connections and published, and it arrives in the
/// peer center's global map, which every node refreshes into RAM every fifteen
/// seconds. Adding them approximates the end to end round trip, because the
/// packet crosses both hops in both directions.
///
/// Three hops or more reports nothing. Walking that chain needs each middle
/// node's route table and this machine only has its own, so the honest answer
/// is that nobody measured it. A Kanpachi room has one relay, the public seed,
/// so this is not a case the product produces today.
///
/// # Why not the route's own path_latency, which is this same sum
///
/// Because it substitutes a flat 500 for an edge the peer center has not heard
/// about yet, and the sum gives nobody a way to tell that apart from a slow
/// link. With a single hop the path cost IS the edge cost, so a member on a
/// direct tunnel reported `500 ms` for as long as the map took to converge.
/// Reading the map directly is what makes the absence visible.
///
/// # Measured, 2026-08-26, two nodes over the public seed
///
/// A Windows host and a guest in WSL, which is behind enough NAT to force the
/// relay. Both sides, instrumented, for the first fifteen seconds:
///
/// ```text
/// mine=Some(138) theirs=None       path_latency=1000
/// mine=Some(139) theirs=Some(144)  path_latency=279
/// ```
///
/// `1000` is two unknown edges at 500 each, and it is what this field used to
/// report while the map was still arriving. Once the far hop lands the two
/// numbers agree: the host summed 146 + 151 = 297 and a plain ping across the
/// tunnel averaged 296.
///
/// The local half is there from the very first read, because it comes off a
/// connection this machine holds. That is the whole gain for a direct member:
/// no waiting, and no 500.
fn measured_rtt_ms(
    route: &Route,
    pairs: &[PeerRoutePair],
    global: &BTreeMap<u32, PeerInfoForGlobalMap>,
) -> Option<i32> {
    // A hop this machine holds, from its own connection stats.
    let mine = |peer_id: u32| -> Option<i32> {
        pairs
            .iter()
            .find(|p| p.route.as_ref().is_some_and(|r| r.peer_id == peer_id))
            .and_then(|p| p.get_latency_ms())
            .map(as_ms)
    };
    // A hop somebody else measured and the center passed along. It reports one
    // direction; latency is close enough to symmetric that EasyTier's own cost
    // calculator falls back to the other one, and so does this.
    let theirs = |src: u32, dst: u32| -> Option<i32> {
        let one = |a: u32, b: u32| {
            global
                .get(&a)
                .and_then(|info| info.direct_peers.get(&b))
                .map(|d| d.latency_ms)
        };
        one(src, dst).or_else(|| one(dst, src)).map(|ms| ms.max(1))
    };
    match route.cost {
        c if c <= 1 => mine(route.peer_id),
        2 => Some(mine(route.next_hop_peer_id)? + theirs(route.next_hop_peer_id, route.peer_id)?),
        _ => None,
    }
}

/// Milliseconds out of EasyTier's float, floored at one.
///
/// A direct tunnel on the same physical LAN measures below a millisecond and
/// would round to zero, which on this wire is the value that means nobody
/// measured it. EasyTier floors its own global map the same way. See
/// [`crate::proto::PeerOut::rtt_ms`].
fn as_ms(ms: f64) -> i32 {
    (ms.round() as i32).max(1)
}

/// Strips a prefix length so that `virtual_ip` is what the protocol says it is.
///
/// # The bug this fixes, seen in a real room
///
/// `NodeInfo.ipv4_addr` carries the configured address WITH its prefix, so this
/// node reported itself as `10.99.61.1/24`. The daemon parses that field with
/// `netip.ParseAddr`, which refuses anything but a bare address, so **every**
/// call to `peers` failed:
///
/// ```text
/// el motor reportó la dirección "10.99.61.1/24", que no es una dirección
/// ```
///
/// Nothing crashed and the room stayed up, which is why it went unnoticed: what
/// broke was the member list, and with it the firewall rules toward members and
/// the inference of whether the host is still present.
///
/// It is fixed here rather than in the daemon on purpose. The daemon's strict
/// parse is right: it is the reason this was visible at all. What was wrong is
/// this side sending something other than what the field promises.
fn bare_addr(s: &str) -> String {
    match s.split_once('/') {
        Some((addr, _)) => addr.to_string(),
        None => s.to_string(),
    }
}

/// How long the pump waits for a new peer's route to carry an address.
///
/// Three seconds against a convergence measured in single digits of seconds on
/// a real host: the guest showed up in the mesh 4.3 seconds after the host had
/// already applied its rules. The cap matters more than the number, because
/// past it the event goes out anyway.
const ESPERA_DE_RUTA: Duration = Duration::from_secs(3);

/// How often it re-reads the route table while waiting.
const SONDEO_DE_RUTA: Duration = Duration::from_millis(100);

/// Waits until a peer's route carries an address, then builds the event.
///
/// # The failure this closes, measured on 2026-08-25 against a real host
///
/// The bus fires `PeerConnAdded` the moment a connection comes up, and
/// [`Engine::peers`] drops every route with no `ipv4_addr` because a node with
/// no address in the room is not a member of it. Between those two facts sits a
/// window: the daemon re-reads the moment the event lands, gets a list without
/// the member who just arrived, writes its firewall rules from that list, and
/// the routes converge seconds later WITHOUT producing another event. Nothing
/// re-reads, and the member's every packet dies in the host's own gate. Three
/// people were locked out of a room for thirty-three hours that way.
///
/// # Why it emits anyway when the wait runs out
///
/// Because a peer that never resolves an address is not a reason to go quiet.
/// The public seed is exactly that: it relays for the room and does not live in
/// its address space, so its route never carries one. Staying silent would trade
/// a late event for no event, which is the same bug wearing different clothes.
/// The reason says which of the two happened.
async fn esperar_direccion(
    api: Option<&std::sync::Arc<dyn easytier::rpc_service::InstanceRpcService>>,
    peer_id: u32,
    motivo: &str,
) -> Event {
    let Some(api) = api else {
        return Event::new(EventKind::PeersChanged, motivo);
    };

    let limite = tokio::time::Instant::now() + ESPERA_DE_RUTA;
    loop {
        let convergio = api
            .get_peer_manage_service()
            .list_route(
                BaseController::default(),
                ListRouteRequest { instance: None },
            )
            .await
            .map(|r| {
                r.routes
                    .iter()
                    .any(|r| r.peer_id == peer_id && r.ipv4_addr.is_some())
            })
            .unwrap_or(false);
        if convergio {
            return Event::new(EventKind::PeersChanged, motivo);
        }
        if tokio::time::Instant::now() >= limite {
            return Event::new(
                EventKind::PeersChanged,
                format!("{motivo}, and its route still has no address"),
            );
        }
        tokio::time::sleep(SONDEO_DE_RUTA).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{bare_addr, measured_rtt_ms};
    use easytier::proto::api::instance::{
        PeerConnInfo, PeerConnStats, PeerInfo, PeerRoutePair, Route,
    };
    use easytier::proto::peer_rpc::{DirectConnectedPeerInfo, PeerInfoForGlobalMap};
    use std::collections::BTreeMap;

    const ME: u32 = 1;
    const SEED: u32 = 2;
    const MEMBER: u32 = 3;

    /// A route with only the three fields this decision reads.
    fn route(peer_id: u32, cost: i32, next_hop: u32) -> Route {
        Route {
            peer_id,
            cost,
            next_hop_peer_id: next_hop,
            ..Default::default()
        }
    }

    /// One pair carrying a live connection with a measurement on it.
    ///
    /// No `default_conn_id`, so `get_latency_ms` takes the lowest of the
    /// connections, which with one connection is that one.
    fn pair_with_stats(peer_id: u32, latency_us: u64) -> PeerRoutePair {
        PeerRoutePair {
            route: Some(route(peer_id, 1, peer_id)),
            peer: Some(PeerInfo {
                peer_id,
                conns: vec![PeerConnInfo {
                    conn_id: "c".to_string(),
                    peer_id,
                    stats: Some(PeerConnStats {
                        latency_us,
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        }
    }

    /// What the center heard: `src` measured `latency_ms` to `dst`.
    fn global(src: u32, dst: u32, latency_ms: i32) -> BTreeMap<u32, PeerInfoForGlobalMap> {
        let mut direct_peers = BTreeMap::new();
        direct_peers.insert(dst, DirectConnectedPeerInfo { latency_ms });
        let mut map = BTreeMap::new();
        map.insert(src, PeerInfoForGlobalMap { direct_peers });
        map
    }

    #[test]
    fn strips_the_prefix_and_leaves_a_bare_address_alone() {
        assert_eq!(bare_addr("10.99.61.1/24"), "10.99.61.1");
        assert_eq!(bare_addr("10.99.61.1"), "10.99.61.1");
        assert_eq!(bare_addr(""), "");
    }

    #[test]
    fn a_direct_member_reports_what_its_own_connection_measured() {
        let pairs = vec![pair_with_stats(MEMBER, 12_400)];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 1, MEMBER), &pairs, &BTreeMap::new()),
            Some(12)
        );
    }

    /// The route exists and no connection has a measurement on it, which is
    /// evidence and not a gap to paper over: the daemon's dial diagnosis reads
    /// exactly this case. Nothing is reported, and the peer center's map is not
    /// consulted for a hop this machine holds itself.
    #[test]
    fn a_direct_member_with_no_live_connection_reports_nothing() {
        let pairs = vec![PeerRoutePair {
            route: Some(route(MEMBER, 1, MEMBER)),
            peer: None,
        }];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 1, MEMBER), &pairs, &global(ME, MEMBER, 40),),
            None
        );
    }

    #[test]
    fn a_relayed_member_adds_the_two_measured_hops() {
        let pairs = vec![pair_with_stats(SEED, 80_000)];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 2, SEED), &pairs, &global(SEED, MEMBER, 81),),
            Some(161)
        );
    }

    /// The center reports one direction. Latency is close enough to symmetric
    /// that EasyTier's own cost calculator falls back to the other one, and so
    /// does this.
    #[test]
    fn a_relayed_member_accepts_the_far_hop_measured_the_other_way_round() {
        let pairs = vec![pair_with_stats(SEED, 80_000)];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 2, SEED), &pairs, &global(MEMBER, SEED, 81),),
            Some(161)
        );
    }

    /// This is the case the flat 500 used to hide.
    #[test]
    fn a_relayed_member_with_the_far_hop_missing_reports_nothing() {
        let pairs = vec![pair_with_stats(SEED, 80_000)];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 2, SEED), &pairs, &BTreeMap::new()),
            None
        );
    }

    /// A tunnel on the same physical LAN measures below a millisecond, and zero
    /// is the value that means "nobody measured it".
    #[test]
    fn a_link_faster_than_a_millisecond_still_reports_one() {
        let pairs = vec![pair_with_stats(MEMBER, 400)];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 1, MEMBER), &pairs, &BTreeMap::new()),
            Some(1)
        );
    }

    /// Walking a longer chain needs each middle node's route table, and this
    /// machine only has its own.
    #[test]
    fn a_path_longer_than_the_relay_reports_nothing() {
        let pairs = vec![pair_with_stats(SEED, 80_000)];
        assert_eq!(
            measured_rtt_ms(&route(MEMBER, 3, SEED), &pairs, &global(SEED, MEMBER, 81),),
            None
        );
    }
}
