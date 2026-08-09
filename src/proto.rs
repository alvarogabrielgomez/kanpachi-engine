//! The wire types: what the daemon sends, and what comes back.
//!
//! # The shape, and why it is this one
//!
//! One JSON object per line, in both directions, over the child process's own
//! pipes. Three kinds of message and no others:
//!
//! - a [`Request`], always carrying an `id`;
//! - a [`Response`], carrying the `id` of the request it answers;
//! - an [`Event`], carrying **no** `id`, pushed whenever the engine has news.
//!
//! The absence of an `id` is what tells the two apart, so nothing has to be
//! guessed from the payload.
//!
//! # Why the command is a nested object and not a flat field
//!
//! The obvious encoding is `{"id":1,"cmd":"host","network_name":…}`. It reads
//! nicer and it cannot be made strict: serde's `flatten` silently disables
//! `deny_unknown_fields`, so a message with a misspelled or extra field would be
//! accepted with that field dropped. This side parses input from a process
//! running as SYSTEM, and the rest of Kanpachi already rejects the whole message
//! on an unknown field rather than ignoring it. Nesting the command keeps that
//! guarantee:
//!
//! ```json
//! {"id":1,"cmd":{"host":{"common":{…},"network_name":"kanpachi-…", …}}}
//! {"id":5,"cmd":{"leave":{}}}
//! ```
//!
//! Every variant carries an object, including the ones with no arguments, so
//! the shape never changes and the Go side can model it as one struct of
//! optional pointers.

use serde::{Deserialize, Serialize};

/// A command from the daemon.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub cmd: Command,
}

/// Carries no arguments. Present so that every variant has an object payload.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Start the real network as the admin node.
    Host(HostArgs),
    /// Enter the lobby, which is a second network living at the same time.
    JoinRendezvous(RendezvousArgs),
    /// Leave ONLY the lobby. Never the room.
    LeaveRendezvous(Empty),
    /// Enter the real network as a temporary node, with a credential.
    Join(GuestArgs),
    /// Leave everything. Idempotent by contract: the daemon calls it on the
    /// error path of creating and of joining, which can run before anything
    /// started.
    Leave(Empty),
    IssueCredential(IssueArgs),
    RevokeCredential(RevokeArgs),
    ListCredentials(Empty),
    Peers(Empty),
    Diagnostics(Empty),
}

/// What every start command needs, whatever network it starts.
///
/// `dev_name` is decided by the daemon and not here. The firewall gate is
/// scoped to an adapter by name, so the side that writes the firewall has to be
/// the side that names the adapter. An engine that picked its own name could
/// hand back an adapter the gate does not cover.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Common {
    pub dev_name: String,
    pub hostname: String,
    /// Seed addresses, ALREADY RESOLVED by the daemon and validated there.
    /// Passing a name would let this process resolve it again, and then the
    /// daemon's check would govern nothing.
    pub peers: Vec<String>,
    #[serde(default)]
    pub mtu: Option<u32>,
    /// Where this process writes `kanpachi-engine.log`. `None` writes nothing.
    ///
    /// # Why the daemon has to say it
    ///
    /// Because "next to the executable" is wrong exactly when the log matters
    /// most: in the portable bundle this binary lives in a temporary directory
    /// that gets deleted on the way out, so the file would die at the moment
    /// somebody was about to send it. It is the same reason the daemon has a
    /// `--log` flag.
    ///
    /// # Why it comes through the protocol and not the environment
    ///
    /// Because the daemon starts this process with an EMPTY environment, on
    /// purpose: every EasyTier flag has an environment twin, so an inherited
    /// environment is a way to switch on capabilities nobody wrote down. The
    /// command channel is the only way in, which is the whole point of it.
    ///
    /// It rides in `Common` because logging starts with the first network and
    /// there is no command that comes earlier.
    #[serde(default)]
    pub log_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostArgs {
    /// A named field and not `#[serde(flatten)]`: serde refuses to combine
    /// flattening with `deny_unknown_fields`, and the strictness is worth more
    /// than the nesting costs.
    pub common: Common,
    /// Name of the REAL network. Derived from random bytes by the daemon.
    pub network_name: String,
    /// Secret of the REAL network. Only the host ever holds this.
    pub network_secret: String,
    /// This node's address inside the room, as `10.0.0.1/24`.
    pub ipv4: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendezvousArgs {
    pub common: Common,
    pub network_name: String,
    pub network_secret: String,
    pub ipv4: String,
}

/// Entering the real network with a credential.
///
/// There is no secret here, and that absence is the point. A temporary node
/// authenticates with the credential keypair and never learns the network
/// secret, which is what makes revoking a credential actually close the door.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestArgs {
    pub common: Common,
    pub network_name: String,
    /// The credential's private key, base64 X25519, exactly as the host's
    /// engine handed it out.
    ///
    /// There is no `credential_id` here even though the daemon holds one. The
    /// engine identifies a temporary node by the public key derived from this
    /// secret, so an id would be a field accepted and ignored, which is the
    /// kind of silence the strict decoding above exists to prevent.
    pub credential_secret: String,
    /// This node's address inside the room, as `10.99.77.2/24`.
    ///
    /// **With the prefix**, like `HostArgs::ipv4` and for the same reason: it
    /// parses into an `Ipv4Inet`, which is an address AND a network.
    ///
    /// # Why the daemon sends it, when it used to be DHCP's job
    ///
    /// Because the host already decided it, wrote it into the credential and
    /// told the guest — and then nobody told the engine, which picked its own
    /// with DHCP. Two algorithms computing the same thing separately, agreeing
    /// only until somebody reconnected.
    ///
    /// Measured on 2026-08-08: the host issued `10.99.29.9` and the adapter
    /// took `10.99.29.2`, because the host counts issued credentials for 24
    /// hours while DHCP counts connected peers. The daemon waits 30 s for the
    /// address in the credential, never gets it, and tears the room down. From
    /// the first failure on, every retry fails the same way: the host's number
    /// climbs and the engine's stays put.
    pub ipv4: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueArgs {
    /// Seconds. Must be greater than zero; EasyTier rejects zero.
    pub ttl_seconds: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeArgs {
    pub credential_id: String,
}

/// The answer to one [`Request`].
#[derive(Debug, Serialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Data>,
}

impl Response {
    pub fn ok(id: u64) -> Self {
        Response { id, ok: true, error: None, data: None }
    }

    pub fn with(id: u64, data: Data) -> Self {
        Response { id, ok: true, error: None, data: Some(data) }
    }

    pub fn failed(id: u64, err: impl std::fmt::Display) -> Self {
        Response { id, ok: false, error: Some(err.to_string()), data: None }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Data {
    Credential(CredentialOut),
    Credentials(Vec<CredentialSummary>),
    Peers(Vec<PeerOut>),
    Diagnostics(DiagnosticsOut),
}

#[derive(Debug, Serialize)]
pub struct CredentialOut {
    pub credential_id: String,
    pub credential_secret: String,
}

#[derive(Debug, Serialize)]
pub struct CredentialSummary {
    pub credential_id: String,
    pub expiry_unix: i64,
}

#[derive(Debug, Serialize)]
pub struct PeerOut {
    /// Empty when the engine knows the peer without knowing its address yet.
    pub virtual_ip: String,
    pub hostname: String,
    /// `direct`, `relay`, or `self`. The daemon maps these onto its own closed
    /// set, so a value it does not know has to be an error there and not a
    /// silent default.
    pub path: &'static str,
    pub rtt_ms: i32,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsOut {
    /// EasyTier's own NAT names, passed through rather than translated. The
    /// engine has no opinion on how a NAT type should read on screen.
    pub nat_kind: String,
    pub udp_blocked: bool,
    pub public_ips: Vec<String>,
    pub virtual_ip: String,
    pub mtu: u32,
}

/// Pushed by the engine, with no request behind it.
///
/// The set is closed on purpose, and it is the daemon's closed set minus one.
/// **There is no `died` event**: the engine cannot report its own death. The
/// adapter raises that one when the child process exits.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Connected,
    PeersChanged,
    Degraded,
    Disconnected,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub event: EventKind,
    /// Why, in text, because a transition logged without its cause is a log
    /// line that helps nobody six months later.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl Event {
    pub fn new(event: EventKind, reason: impl Into<String>) -> Self {
        Event { event, reason: Some(reason.into()) }
    }
}

/// Either half of what this process writes on stdout.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Outgoing {
    Response(Response),
    Event(Event),
}
