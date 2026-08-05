//! Link probe. This is not the engine yet.
//!
//! # What this binary is for
//!
//! It answers one question, and nothing else: **does the `easytier` crate link
//! when consumed as a git dependency from a foreign workspace?**
//!
//! The doubt is concrete, not a hunch. `easytier/build.rs` at tag `v2.6.4`
//! emits a **relative** link search path:
//!
//! ```text
//! println!("cargo:rustc-link-search=native=easytier/third_party/x86_64/");
//! ```
//!
//! That path is resolved from the directory the linker runs in, which for us is
//! this repository and not EasyTier's checkout. So it points at a directory
//! that does not exist here. Whether that is fatal depends on whether anything
//! in the tree actually asks for a library that lives in there.
//!
//! Writing the protocol, the config generator and the event bridge first, and
//! only then discovering the tree cannot link, would cost all of that work. So
//! this comes first and everything else waits on it.
//!
//! The call below is deliberate: naming a type is not enough, because an unused
//! `--extern` can be dropped before the linker ever sees it. Calling a trait
//! method forces the crate into the final link.

use easytier::common::config::{ConfigLoader, TomlConfigLoader};

fn main() {
    let cfg = TomlConfigLoader::default();
    println!("kanpachi-engine link probe: inst_name={}", cfg.get_inst_name());
}
