//! Repairs a link search path that EasyTier emits relative to the wrong place.
//!
//! # The failure this exists to fix, measured
//!
//! `easytier/build.rs` at tag `v2.6.4` emits, verbatim:
//!
//! ```text
//! println!("cargo:rustc-link-search=native=easytier/third_party/x86_64/");
//! ```
//!
//! That path is **relative**. Cargo runs the linker from the root of the package
//! being built, which here is this repository and not EasyTier's checkout, so it
//! resolves to a directory that does not exist. The whole dependency tree
//! compiles and the final link fails:
//!
//! ```text
//! "/LIBPATH:easytier/third_party/x86_64/" ... "Packet.lib"
//! LINK : fatal error LNK1181: cannot open input file 'Packet.lib'
//! ```
//!
//! `Packet.lib` is the import library for `Packet.dll`, which EasyTier imports
//! hard (`PacketGetAdapterNames`, `PacketSendPacket`). It is not optional and no
//! cargo feature removes it.
//!
//! # Why a search path and not a copy of the file
//!
//! The obvious fix is to create `easytier/third_party/x86_64/` in this
//! repository so the relative path resolves. That means committing a
//! third-party binary of WinPcap/Npcap lineage into a public repository, whose
//! redistribution terms nobody here has reviewed. Pointing the linker at the
//! copy cargo already unpacked costs the same and redistributes nothing.
//!
//! # Why the location is searched for instead of written down
//!
//! Cargo owns the checkout path and it contains a hash of the source URL that is
//! an implementation detail. So the roots are probed in order, and the first one
//! holding the library wins. `KANPACHI_ENGINE_LINK_SEARCH` overrides everything,
//! for vendored trees and for anyone whose layout is not one of these.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=KANPACHI_ENGINE_LINK_SEARCH");

    if let Ok(dir) = env::var("KANPACHI_ENGINE_LINK_SEARCH") {
        println!("cargo:rustc-link-search=native={dir}");
        return;
    }

    // Same architecture mapping as easytier's own build script.
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let leaf = match arch.as_str() {
        "x86_64" => "x86_64",
        "x86" => "i686",
        "aarch64" => "arm64",
        other => {
            println!("cargo:warning=unknown target arch {other}, not patching the link search path");
            return;
        }
    };

    match find_third_party(leaf) {
        Some(dir) => println!("cargo:rustc-link-search=native={}", dir.display()),
        None => panic!(
            "could not find EasyTier's third_party/{leaf} directory, which holds Packet.lib.\n\
             The link will fail without it, because easytier's build script points at that \
             directory with a path relative to the wrong root.\n\
             Set KANPACHI_ENGINE_LINK_SEARCH to the directory that contains Packet.lib."
        ),
    }
}

/// Probes the plausible cargo roots and returns the first that holds the library.
fn find_third_party(leaf: &str) -> Option<PathBuf> {
    for root in cargo_roots() {
        let checkouts = root.join("git").join("checkouts");
        let Ok(entries) = std::fs::read_dir(&checkouts) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with("easytier-") {
                continue;
            }
            // One directory per checked-out revision.
            let Ok(revs) = std::fs::read_dir(entry.path()) else {
                continue;
            };
            for rev in revs.flatten() {
                let dir = rev.path().join("easytier").join("third_party").join(leaf);
                if dir.join("Packet.lib").is_file() {
                    return Some(dir);
                }
            }
        }
    }
    None
}

/// The candidate cargo homes, most authoritative first.
fn cargo_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();

    if let Ok(home) = env::var("CARGO_HOME") {
        out.push(PathBuf::from(home));
    }
    // `CARGO` is documented as the path to the cargo binary running the build.
    // Under the usual layout its grandparent is the cargo home; under a
    // toolchain-local cargo it is not, which is why this is a candidate and not
    // the answer.
    if let Ok(cargo) = env::var("CARGO") {
        if let Some(dir) = Path::new(&cargo).parent().and_then(Path::parent) {
            out.push(dir.to_path_buf());
        }
    }
    if let Ok(profile) = env::var("USERPROFILE") {
        out.push(Path::new(&profile).join(".cargo"));
    }
    if let Ok(home) = env::var("HOME") {
        out.push(Path::new(&home).join(".cargo"));
    }
    out
}
