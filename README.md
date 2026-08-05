# kanpachi-engine

Part of **[Kanpachi Protection](https://github.com/alvarogabrielgomez/kanpachi/blob/main/kanpachi-protection.md)**:
*everything the game did not ask for is closed on the virtual adapter.*

This is the network engine of
[Kanpachi](https://github.com/alvarogabrielgomez/kanpachi): a small Windows
binary that builds the encrypted peer-to-peer network and **listens on nothing**.

Its share of that promise is narrow and worth stating plainly. **The engine
decides nothing.** It moves packets. What may be reached is decided and written
by the daemon, so a compromise of this binary cannot open the machine, and this
binary offers no way to be told otherwise.

It is built on
[kanpachi/EasyTier](https://github.com/alvarogabrielgomez/EasyTier), Kanpachi's
fork of [EasyTier](https://github.com/EasyTier/EasyTier), pinned to the tag
[`v2.6.4-kanpachi.1`](https://github.com/alvarogabrielgomez/EasyTier/tree/v2.6.4-kanpachi.1).
Upstream unconditionally writes Windows Firewall rules that open the virtual
adapter, and the fork is upstream with those two calls removed and nothing else.
[Why, in detail.](#the-firewall-and-why-the-dependency-is-a-fork)

It takes commands on stdin and writes answers and events on stdout. It has no
port, no named pipe, no config file, and it accepts no command-line arguments at
all. It is not meant to be run by hand.

## Why this exists

The official `easytier-core.exe` opens an administration portal. It has **no
authentication of any kind**, and its default binds every interface:

| Process | Listening TCP sockets |
|---|---|
| `easytier-core.exe` v2.6.4 | `0.0.0.0:15888` |
| `kanpachi-engine.exe` | none |

Through that portal any local process can issue credentials for the network, add
peers, forward ports, and ask for the network secret in cleartext. Authentication
was requested upstream in issue #925 and deliberately declined in PR #929, in
favour of an IP allowlist that filters *after* accept and whose default already
includes `127.0.0.0/8`, which is no barrier to another process on the same
machine.

**The portal is not part of the library.** `ApiRpcServer::new` is constructed in
exactly one place in the whole tree, inside upstream's command-line binary. A
program that drives the library and never writes that line does not get a portal.
There is no flag to forget:

```
easytier/src/core.rs:1340   ApiRpcServer::new(...)   ← the CLI, and nowhere else
easytier/src/launcher.rs                            ← no matches
```

So this repository is not a patch or a wrapper. It is a different program that
uses the same library, written so that the capability is absent rather than
turned off.

## What it deliberately cannot do

Some of these are configuration, and some are the absence of code. The
difference matters: a flag can be flipped at runtime, and a missing feature
cannot.

**Removed from the binary** (cargo features left out, so the code is not
compiled in):

| Capability | Why |
|---|---|
| `magic-dns` | Rewrites the machine's DNS and opens a loopback socket. That socket is exactly what an early "does this thing listen" measurement missed, because it ran with `no_tun` |
| `socks5` | A proxy into the virtual network |
| `wireguard` | `boringtun`, which exists to serve a VPN portal. Not the tunnel encryption, which stays |
| `faketcp` | A transport nothing here asks for |

**Stated explicitly in the configuration**, including the ones whose upstream
default already matches, so that a default changing upstream cannot switch a
forbidden capability on without somebody writing it here:

```
disable_upnp               true    the user's router is never touched
enable_exit_node           false   never an exit node
accept_dns                 false   the machine's DNS is not modified
enable_udp_broadcast_relay false   would capture the home network's traffic
private_mode               true    refuses peers from other networks
listeners                  empty   the client never listens on a public port
mapped_listeners           none    publishing reachable addresses is listening
exit_nodes, proxy_cidrs    empty   no subnet routing
socks5_portal              none
port_forwards              empty
credential_file            none    credentials stay in memory, never on disk
```

**Still in the binary and worth naming:** `windivert`, the packet-capture
driver, is an unconditional dependency on Windows x86 and x86_64. No combination
of cargo features removes it. That is precisely why Kanpachi's containment rests
on the firewall rather than on this engine being incapable.

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
upstream `v2.6.4` with those two calls removed and nothing else. Removing them
is safe: upstream already treated the failure of both as non-fatal, logging a
warning and continuing.

The claim is meant to be checked rather than believed, and that is also why the
engine lives in its own repository instead of inside the fork:

```
git diff v2.6.4 v2.6.4-kanpachi.1     # one file, nine deletions
```

See the fork's
[FORK.md](https://github.com/alvarogabrielgomez/EasyTier/blob/kanpachi/FORK.md),
which carries the changelog against upstream.

## The command channel

Stdin is the **only** input of this program. Never a port, never a named pipe,
never a watched file, never a signal.

That is not a promise this code keeps by being careful; it is what the operating
system enforces. The pipes are **anonymous**: no name, no path, no address.
Connecting to them is not forbidden, it is an operation that does not exist. The
two ends live as handles inside the daemon and inside this process. A port or a
named pipe would be doors, a door needs a lock, and a lock can be written
wrongly, which is what happened to port 15888.

Running `kanpachi-engine.exe` by hand starts a *different*, empty instance whose
stdin is the terminal of whoever launched it. It knows no room's secret and
touches no other instance's tunnels.

### The shape

One JSON object per line, both directions. Three kinds of message and no others:

| Kind | Has `id` |
|---|---|
| Request, daemon to engine | yes |
| Response, engine to daemon | yes, the same one |
| Event, engine to daemon, unsolicited | **no** |

The absence of an `id` is what tells a response from an event, so nothing is
guessed from the payload. Decoding is **strict**: an unknown field rejects the
whole message rather than being dropped.

```jsonc
→ {"id":1,"cmd":{"host":{"common":{"dev_name":"kanpachi0","hostname":"alvaro",
                 "peers":["tcp://203.0.113.9:11010"]},
                 "network_name":"kanpachi-a1b2","network_secret":"9f3a…",
                 "ipv4":"100.87.4.1/24"}}}
← {"id":1,"ok":true}
← {"event":"peers_changed","reason":"somebody joined"}
→ {"id":2,"cmd":{"issue_credential":{"ttl_seconds":3600}}}
← {"id":2,"ok":true,"data":{"credential":{"credential_id":"…","credential_secret":"…"}}}
→ {"id":3,"cmd":{"leave":{}}}
```

Commands: `host`, `join_rendezvous`, `leave_rendezvous`, `join`, `leave`,
`issue_credential`, `revoke_credential`, `list_credentials`, `peers`,
`diagnostics`.

Events: `connected`, `peers_changed`, `degraded`, `disconnected`. There is no
`died`: a process cannot report its own death, so the daemon raises that one
when the child exits.

**Closing stdin shuts the engine down**, cleanly and on purpose. That is the
normal way to stop it.

### Two networks at once

The host lives in two networks simultaneously: the room, and a public, throwaway
lobby that anyone holding the invite code can derive. They are two separate
network instances with two adapters, `kanpachi0` and `kanpachi1`, so that
`leave_rendezvous` can drop the lobby while the room stays up.

The adapter names are chosen by the daemon and sent in each command. The
firewall gate is scoped to an adapter **by name**, so the side that writes the
firewall has to be the side that names the adapter; an engine picking its own
name could hand back one the gate does not cover.

## Building

Windows only, x86_64, MSVC toolchain. Rust is pinned by `rust-toolchain.toml` to
the version upstream EasyTier builds with.

```powershell
.\scripts\build.ps1                     # builds into C:\kt
.\scripts\build.ps1 -Stage C:\kt\stage  # and copies the binary next to its DLLs
```

The script exists because this dependency tree fails in six different ways on a
machine that has everything installed and nothing exported, and each failure
names something other than the real cause. It locates and sets up MSVC
(`vcvars64`), `protoc`, `libclang` for `kcp-sys`, and 7-Zip for `thunk-rs`, puts
the target directory somewhere short because `cl.exe` is not long-path aware,
and says which tool is missing instead of letting the compiler guess. A cold
build takes roughly twenty minutes.

`build.rs` in this repository repairs one more trap: the library's own build script
emits a **relative** link search path for `Packet.lib`, which resolves against
the consuming package and therefore fails to link. This one points the linker at
the copy cargo already unpacked, rather than committing a third-party binary
here.

### Runtime files

These must sit next to the executable. They are not built here and not
redistributed by this repository; see [NOTICE.md](NOTICE.md).

| File | Note |
|---|---|
| `Packet.dll` | **A hard import.** Without it the process does not start, and Windows only says `0xC0000135` without naming what is missing |
| `wintun.dll` | The virtual adapter, loaded at runtime |
| `WinDivert64.sys` | Pulled in by the `windivert` dependency |

Creating a virtual adapter is privileged, so the engine runs elevated. In
Kanpachi it is a child of a service running as SYSTEM, inside a Job Object with
`KILL_ON_JOB_CLOSE`, so that a daemon dying without running any cleanup still
takes the engine and its network down with it.

## Licence

LGPL-3.0-or-later, because it links the forked library statically and is
therefore a *Combined Work* under section 4. [NOTICE.md](NOTICE.md) records every
third-party component and explains how to replace the EasyTier part with a
version of your own and relink, which that section entitles you to do.
