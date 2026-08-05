# Third-party notices

`kanpachi-engine` is the network engine of
[Kanpachi](https://github.com/alvarogabrielgomez/kanpachi). This file records
what it includes, under which licence, and how to exercise the rights those
licences grant you.

Written in English, unlike Kanpachi's design documents, because it is addressed
to anyone who redistributes the binary rather than to the people who build it.

## EasyTier, as a modified library

- **Upstream**: [EasyTier](https://github.com/EasyTier/EasyTier)
- **What is linked**: a **fork**,
  [alvarogabrielgomez/EasyTier](https://github.com/alvarogabrielgomez/EasyTier),
  tag [`v2.6.4-kanpachi.1`](https://github.com/alvarogabrielgomez/EasyTier/tree/v2.6.4-kanpachi.1)
- **Licence**: GNU Lesser General Public License, version 3 or later

**The fork is upstream `v2.6.4` with two calls removed and nothing else.** Both
wrote permanent Windows Firewall ALLOW rules, by COM, from inside network
startup: one opened the virtual adapter to all traffic, and one granted the
executable inbound "any protocol" on every interface of the machine. Neither
could be disabled through a cargo feature, a config field or an environment
variable. The reasoning is in
[FORK.md](https://github.com/alvarogabrielgomez/EasyTier/blob/kanpachi/FORK.md),
and the claim is meant to be verified rather than believed:

```
git diff v2.6.4 v2.6.4-kanpachi.1
```

`kanpachi-engine` links that library **statically**. Under LGPL-3.0 this binary
is therefore a *Combined Work* in the sense of section 4, and the licence
requires that you be able to replace the EasyTier part with a version of your
own and relink.

**How to do that.** `Cargo.toml` in the root of this repository pins the exact
revision by tag. Point it at your own checkout or fork instead:

```toml
[dependencies]
easytier = { path = "../your-easytier/easytier" }
```

and run `cargo build --release`. Nothing in this repository is obfuscated,
generated, or withheld: the complete corresponding source of the Combined Work
is this repository plus the fork above, whose own source is published in full.

Full licence texts are in this repository:

- [`LICENSE`](LICENSE) — LGPL-3.0
- [`COPYING.GPL`](COPYING.GPL) — GPL-3.0

Both are needed. The LGPL-3.0 is written as a set of additional permissions on
top of the GPL-3.0 and does not stand on its own. EasyTier's own repository does
not ship the GPL text, so this copy comes from
<https://www.gnu.org/licenses/gpl-3.0.txt>.

## Pulled in at build time, linked into the binary

EasyTier's `build.rs` calls `thunk::thunk()` unconditionally on Windows x86 and
x86_64, with `features = ["win7"]`. That downloads two third-party artefacts over
the network **during compilation** and links them into the output:

| Component | Version | What is linked |
|---|---|---|
| [VC-LTL5](https://github.com/Chuyu-Team/VC-LTL5) | 5.2.2 | Import libraries for target platform `6.0.6000.0`, x64 |
| [YY-Thunks](https://github.com/Chuyu-Team/YY-Thunks) | 1.1.7 | `objs/x64/YY_Thunks_for_Win7.obj` |

Their only purpose is to let the binary run on Windows 7. Kanpachi targets
Windows 10 and 11, so they add nothing here and ship anyway. Removing them is
now possible in principle, since a fork already exists, and it has not been done
because nothing about them is known to be harmful.

**Their licence terms have not been reviewed yet.** They are listed so that
whoever distributes a build knows they are in it.

## Runtime components that are not built here

The compiled engine needs these at runtime on Windows. They are **not** part of
this repository and are **not** redistributed by it. They are listed because
anyone shipping the engine to end users will be distributing them too, and each
carries its own terms:

| File | Origin | Note |
|---|---|---|
| `wintun.dll` | [Wintun](https://www.wintun.net/), WireGuard project | The virtual adapter. Loaded at runtime, so it does not appear in the import table |
| `Packet.dll` | WinPcap / Npcap lineage | **A hard import.** The engine does not start without it. The most restrictive of the three; its terms must be reviewed before any public distribution |
| `WinDivert64.sys` | [WinDivert](https://reqrypt.org/windivert.html) | Pulled in by the `windivert` crate, which is an unconditional dependency on Windows x86/x64 and cannot be removed by any cargo feature combination |

That last row is worth stating plainly: `windivert` is not a feature. No build
configuration of this engine can leave the packet-capture capability out of the
binary. What EasyTier's `enable_udp_broadcast_relay` flag decides is whether the
capability is **used**, never whether it ships.
