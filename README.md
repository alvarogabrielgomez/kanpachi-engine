# kanpachi-engine

Part of **[Kanpachi Protection](https://github.com/alvarogabrielgomez/kanpachi/blob/main/kanpachi-protection.md)**:
*everything the game did not ask for is closed on the virtual adapter.*

This is the network engine of
[Kanpachi](https://github.com/alvarogabrielgomez/kanpachi): a small binary,
Windows and Linux, that builds the encrypted peer-to-peer network and **listens
on nothing**.

Its share of that promise is narrow. **The engine decides nothing.** It moves
packets. The daemon decides what may be reached and writes it, so a compromise
of this binary cannot open the machine, and this binary offers no way to be told
otherwise.

It takes commands on stdin and writes answers and events on stdout. It has no
port, no named pipe, no config file, and it accepts no command-line arguments at
all. It is not meant to be run by hand.

## Why this exists

The official `easytier-core.exe` opens an administration portal. It has **no
authentication of any kind**, and its default binds every interface:

| Process | Listening TCP sockets |
|---|---|
| `easytier-core.exe` v2.6.4 | `0.0.0.0:15888` |
| `kanpachi-engine` | none |

Through that portal any local process can issue credentials for the network, add
peers, forward ports, and ask for the network secret in cleartext. Somebody
asked upstream for authentication in issue #925, and upstream declined it in PR
#929 in favour of an IP allowlist that filters *after* accept and whose default
already includes `127.0.0.0/8`, which is no barrier to another process on the
same machine.

**The portal is built in one place, and nothing this program calls can reach
it.** A **private** function, `run_main`, constructs `ApiRpcServer::new`. The
only public function of that whole module is `core::main`, the command-line
entry point that parses `argv` and returns an `ExitCode`. This engine drives the
library through `launcher::NetworkInstance` and never names `core` at all:

```
easytier/src/core.rs:1343   ApiRpcServer::new(...)   <- inside private run_main
easytier/src/core.rs:1549   pub async fn main()      <- the CLI entry, its only way in
grep -rn "::core::" src/                             <- no matches here
```

This repository holds a different program that uses the same library, written so
that the capability is unreachable rather than turned off. It is not a patch,
and it is not a wrapper.

## What it cannot do

Some of these are configuration, and some are the absence of code. The
difference matters: a flag can be flipped at runtime, and a missing feature
cannot.

**Removed from the binary**, the four defaults left out by
`default-features = false`, so the code is not compiled in:

| Capability | Why |
|---|---|
| `magic-dns` | Rewrites the machine's DNS and opens a loopback socket. That socket is what an early "does this thing listen" measurement missed, because it ran with `no_tun` |
| `socks5` | A proxy into the virtual network |
| `wireguard` | `boringtun`, which exists to serve a VPN portal. Not the tunnel encryption, which stays |
| `faketcp` | A transport nothing here asks for |

**Written into the configuration**, including the ones whose upstream default
already matches, so that a default changing upstream cannot switch a forbidden
capability on without somebody writing it here:

```
disable_upnp               true    the user's router is never touched
enable_exit_node           false   never an exit node
accept_dns                 false   the machine's DNS is not modified
enable_udp_broadcast_relay false   would capture the home network's traffic
private_mode               true    refuses peers from other networks
listeners                  empty   the client never listens on a public port
mapped_listeners           none    publishing reachable addresses is listening
exit_nodes, proxy_cidrs    empty   no subnet routing
socks5_portal, port_forwards, credential_file    none
```

**Still in the binary:** `windivert`, the packet-capture driver, is an
unconditional dependency on Windows x86 and x86_64, and no combination of cargo
features removes it. Kanpachi's containment therefore rests on the firewall and
not on this engine being incapable. On Linux it is not in the dependency graph
at all.

### The firewall, and why the dependency is a fork

**Upstream** writes permanent Windows Firewall ALLOW rules while creating the
virtual adapter, on the library path as well as the CLI one: one rule set opens
the virtual interface to all traffic, and another grants the running executable
inbound "any protocol" on **every** interface of the machine. Neither can be
disabled through a feature, a config field or an environment variable, and both
outlive the process, a reboot and an uninstall.

Kanpachi opens only the ports the active game profile asks for, only toward the
addresses of the members present in the room. An allow-all on the same interface
undoes that in the same layer Kanpachi uses to grant access, and the second rule
cannot be covered by Kanpachi's firewall gate at all: that gate is scoped to the
virtual adapter by design, which is the invariant that stops a hard block from
taking down the user's home network.

Hence the fork,
[kanpachi/EasyTier](https://github.com/alvarogabrielgomez/EasyTier), which is
upstream `v2.6.4` with those two calls removed, plus `renew_credential`, which
pushes a credential's expiry forward without reissuing it. Removing the two is
safe: upstream already treated the failure of both as non-fatal, logging a
warning and continuing.

**It is pinned to the tag**
[`v2.6.4-kanpachi.1`](https://github.com/alvarogabrielgomez/EasyTier/tree/v2.6.4-kanpachi.1).
A tag holds still where a branch would move under the build. `Cargo.lock`
records the commit it resolved to, and every build here passes `--locked`, so a
stale lock fails the build instead of regenerating itself in silence. To move to
a newer fork, cut the next tag in the series and write it down here.

The claim is meant to be checked rather than believed, and that is also why the
engine lives in its own repository instead of inside the fork:

```
git diff v2.6.4 v2.6.4-kanpachi.1 -- "*.rs" "*.proto"
```

The fork's
[FORK.md](https://github.com/alvarogabrielgomez/EasyTier/blob/kanpachi/FORK.md)
carries the changelog against upstream.

## The command channel

Stdin is the **only** input of this program. Never a port, never a named pipe,
never a watched file, never a signal.

That is not a promise this code keeps by being careful; it is what the operating
system enforces. The pipes are **anonymous**: no name, no path, no address.
Connecting to them is not forbidden, it is an operation that does not exist. The
two ends live as handles inside the daemon and inside this process. A port or a
named pipe would be doors, a door needs a lock, and somebody has to write that
lock right. On port 15888 nobody did. Running the binary by hand starts a
*different*, empty instance whose stdin is the terminal of whoever launched it:
it knows no room's secret and touches no other instance's tunnels.

One JSON object per line, both directions, and three kinds of message with no
others: a request from the daemon and its response both carry the same `id`, and
an unsolicited event carries none. That absence is what tells them apart, so
nothing is guessed from the payload. Decoding is **strict**: an unknown field
rejects the whole message rather than being dropped.

```jsonc
-> {"id":1,"cmd":{"host":{"common":{"dev_name":"kanpachi0", ...}, ...}}}
<- {"id":1,"ok":true}
<- {"event":"peers_changed","reason":"somebody joined"}
-> {"id":2,"cmd":{"issue_credential":{"ttl_seconds":3600}}}
<- {"id":2,"ok":true,"data":{"credential":{"credential_id":"...", ...}}}
```

Commands: `host`, `join_rendezvous`, `leave_rendezvous`, `join`, `leave`,
`issue_credential`, `renew_credential`, `revoke_credential`, `list_credentials`,
`peers`, `diagnostics`. Their exact arguments are the types in
[`src/proto.rs`](src/proto.rs), which is the only authority on the wire format.

Events: `connected`, `peers_changed`, `degraded`, `disconnected`. There is no
`died`: a process cannot report its own death, so the daemon raises that one
when the child exits. **Closing stdin shuts the engine down**, which is the
normal way to stop it.

### Two networks at once

The host lives in both: the room, and a public, throwaway lobby that anyone
holding the invite code can derive. They are two separate
network instances with two adapters, `kanpachi0` and `kanpachi1`, so that
`leave_rendezvous` can drop the lobby while the room stays up.

The adapter names are chosen by the daemon and sent in each command, because the
firewall gate is scoped to an adapter **by name**: the side that writes the
firewall has to be the side that names the adapter, or the engine could hand
back one the gate does not cover.

## Which engine is this

Every build seals its own identity into the file, so the question can be
answered without running the binary and without trusting a filename:

```
KANPACHI-ENGINE-BUILD-ID{0.1.0+g<commit>}
KANPACHI-ENGINE-LIB{easytier@v2.6.4-kanpachi.1}
```

Both are greppable off the file on either platform, and the same values reach
the startup banner on stderr, the Windows `ProductVersion`, and the
`engine_build` and `engine_lib` fields of the `diagnostics` response, which name
the **running process** rather than a file on disk. A build that cannot know its
commit says `unknown` instead of guessing, and a build from a dirty tree says
so.

A `v*` tag runs the full checks on **both** platforms and publishes the binaries
that passed, never a recompilation of the same commit somewhere else.
Each release carries `kanpachi-engine.exe`, `kanpachi-engine` and
`SHA256SUMS-engine`, and quotes its [CHANGELOG.md](CHANGELOG.md) section as the
release body, which is why the changelog is in English. A version with an empty
section fails the publish on purpose, and a tag that does not match `version` in
`Cargo.toml` stops the run before anything is built.

Kanpachi consumes this through its own `engine.pin`, which records the tag and
both SHA256s and refuses to package anything that does not match. The consumer,
not this repository, decides when to adopt a new engine.

## Building

Each artifact is built **on** the system it runs on. There is no cross-compile
in either direction, which is why there are two scripts and no flag joining
them. Rust is pinned by `rust-toolchain.toml` to the version upstream EasyTier
builds with.

```powershell
.\scripts\build.ps1 -Stage C:\kt\stage  # builds into C:\kt, stages next to its DLLs
```

```bash
scripts/build-linux.sh --stage ./out    # target dir under $HOME/.cache
```

Use the scripts. This dependency tree fails in several ways on a machine that
has everything installed and nothing exported, and each failure names something
other than the real cause; the scripts set up what is needed and say which tool
is missing instead of letting the compiler guess. Each one documents its own
traps in its header.

**The published Linux binary is built on Ubuntu 22.04 on purpose.** The glibc it
links against is the floor of what can run it, and 22.04 is what a VPS most
often has. Building on 24.04 produces a binary that does not start there.

### What it needs at runtime

On **Windows**, `Packet.dll`, `wintun.dll` and `WinDivert64.sys` must sit next to
the executable. They are not built here and not redistributed by this
repository; [NOTICE.md](NOTICE.md) says where each comes from and under what
terms. Without `Packet.dll` the process does not start, and Windows only says
`0xC0000135` without naming what is missing.

On **Linux**, nothing sits beside it. The adapter comes from `/dev/net/tun`, and
addresses and routes are programmed over netlink, so what it needs is the
`CAP_NET_ADMIN` capability, which Kanpachi's systemd unit grants.

Creating a virtual adapter is privileged on both. In Kanpachi the engine is a
child of a service running as SYSTEM or as root; on Windows it also lives inside
a Job Object with `KILL_ON_JOB_CLOSE`, so that a daemon dying without running
any cleanup still takes the engine and its network down with it.

## Licence

LGPL-3.0-or-later, because it links the forked library statically and is
therefore a *Combined Work* under section 4. [NOTICE.md](NOTICE.md) records every
third-party component and explains how to replace the EasyTier part with a
version of your own and relink, which that section entitles you to do.
