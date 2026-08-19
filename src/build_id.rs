//! What this binary is, sealed at compile time.
//!
//! # Why a sentinel string and not only a value
//!
//! Linux has no VERSIONINFO: given a bare `kanpachi-engine` file there is no
//! standard place to ask what it is. [`BUILD_MARK`] is a self-delimiting
//! literal that survives into the binary, so `kanpachi version`, `doctor` and
//! a plain `grep` can read the id off the FILE without executing it. The
//! runtime reference below is what keeps the linker from discarding it.
//!
//! The value comes from `build.rs` (`build_id()` there documents the shape and
//! the honesty rules: the version is Cargo.toml's, the provenance is the
//! commit, and a build that cannot know says `unknown` instead of guessing).

/// The build id: `0.1.0+g839f98e2c1d4` or `0.1.0+g839f98e2c1d4.dirty` or
/// `0.1.0+unknown`.
pub const BUILD: &str = env!("KANPACHI_ENGINE_BUILD");

/// The same id, wrapped so it can be found inside the compiled file:
/// `KANPACHI-ENGINE-BUILD-ID{0.1.0+g839f98e2c1d4}`.
pub const BUILD_MARK: &str = concat!(
    "KANPACHI-ENGINE-BUILD-ID{",
    env!("KANPACHI_ENGINE_BUILD"),
    "}"
);
