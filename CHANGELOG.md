# Changelog

What changed in each release of the Kanpachi engine, for whoever consumes it —
which is Kanpachi's release pipeline first, and a person debugging a shipped
binary second.

The format is [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
versions follow [SemVer](https://semver.org/). Same rules as Kanpachi's own
changelog, which is the reference: one line per entry, imperative mood, linking
its own commit, written in the same commit as the change. In English, because
the release body quotes it verbatim.

What counts as noticeable here is different from an app's: the consumer is a
pipeline and a debugger, so a new stdin command, a changed event, a new field
in the diagnostics or anything that alters the binary's identity all qualify.
Pure refactors and CI plumbing stay in commit messages.

## Unreleased

## [0.1.0] - 2026-08-19

### Added

- Say what this binary IS: every build seals `<version>+<commit>` into a stderr banner at startup, a `KANPACHI-ENGINE-BUILD-ID{...}` sentinel readable off the file without running it, the Windows ProductVersion, and an `engine_build` field in the diagnostics naming the running process. A build that cannot know its commit says so instead of guessing ([9486f08](https://github.com/alvarogabrielgomez/kanpachi-engine/commit/9486f08))
- Publish what passed: a `v*` tag runs the full customs on BOTH platforms — the Linux half did not exist, so the engine inside every published .deb had never been checked by anything — and releases the exact binaries that passed, with `SHA256SUMS-engine` for Kanpachi's `engine.pin` to verify against ([9486f08](https://github.com/alvarogabrielgomez/kanpachi-engine/commit/9486f08))

### Changed

- Pin easytier by tag (`v2.6.4-kanpachi.1`) instead of a branch, and refuse silent drift: `--locked` on every build makes Cargo.lock binding, so the fork commit inside the binary is always one somebody wrote down ([9486f08](https://github.com/alvarogabrielgomez/kanpachi-engine/commit/9486f08))

### Fixed

- Bring the customs back from the dead: the pinned toolchain installed without clippy, so every run died before checking anything and three consecutive commits shipped inside Kanpachi installers ungated. The toolchain file now declares its components, which also revived `cargo fmt --check` — it sat behind clippy and had never run either ([9486f08](https://github.com/alvarogabrielgomez/kanpachi-engine/commit/9486f08))

[0.1.0]: https://github.com/alvarogabrielgomez/kanpachi-engine/releases/tag/v0.1.0
