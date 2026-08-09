//! Turning EasyTier's own logging on, which used to be off entirely.
//!
//! # What was being thrown away
//!
//! Nothing in this binary ever installed a `tracing` subscriber, so every
//! `tracing::info!`, `warn!` and `error!` inside EasyTier went nowhere. That is
//! the routing table changing, DHCP picking an address, handshakes failing and
//! relay decisions — the exact set of facts that a room which does not connect
//! is made of.
//!
//! What that cost, measured on 2026-08-08: a host had a guest inside for twenty
//! minutes, the guest saw two members and the host saw one, and neither log had
//! a single line about it. It was deduced by omission — no `entró a la sala`,
//! the firewall rules sitting at two — instead of read.
//!
//! # The trap: EasyTier's console logger writes INFO to STDOUT
//!
//! And **stdout is the command channel**, one JSON object per line. In
//! `console_layers` the split is `WARN` and worse to stderr, and
//! `filter_fn(|m| *m.level() > Level::WARN)` — INFO, DEBUG, TRACE — to
//! `stdout`. Turning the console logger on with its default level would put log
//! text in the middle of the protocol and corrupt it.
//!
//! So the console level is set to `off` **explicitly**, and stays written down
//! even though not calling `init` at all would also leave it off. That is the
//! same rule this crate already states for every forbidden capability: written
//! here, so that a default changing upstream cannot switch it on without
//! somebody editing this file.

use std::sync::Once;

use easytier::common::config::{ConsoleLoggerConfig, FileLoggerConfig, LoggingConfig};

/// The log's name, next to the daemon's own `kanpachi.log`.
///
/// **With the extension, because this string IS the filename.** EasyTier joins
/// `dir` and `file` verbatim and defaults to `easytier.log`, so a bare
/// `kanpachi-engine` produced a file with no extension that Windows offers to
/// open with a program picker. Measured on 2026-08-09 in the portable bundle.
const FILE: &str = "kanpachi-engine.log";

/// Eight megabytes and two previous copies.
///
/// **Bigger than the daemon's, and that is not sloppiness: this file fills much
/// faster.** EasyTier logs one INFO line per multicast packet it cannot route,
/// and a Windows machine emits SSDP and mDNS constantly, so `no peer id for ip:
/// 239.255.255.250` is most of the volume. Measured: 266 KB in fifteen minutes,
/// about a megabyte an hour, which at two megabytes wraps in the middle of the
/// session somebody is trying to explain.
///
/// The noise is NOT filtered out, on purpose. EasyTier parses this level with
/// `s.parse::<LevelFilter>().unwrap()`, so it takes a bare level and an
/// `EnvFilter` directive like `easytier::peers::peer_manager=warn` panics
/// rather than narrowing anything. And silencing that target wholesale would
/// take the peer routing lines with it, which are exactly what a host that
/// cannot see its guest is diagnosed with. So every line is kept and more of
/// them are held.
const SIZE_MB: u64 = 8;
const COUNT: usize = 3;

static UNA_VEZ: Once = Once::new();

/// Installs the subscriber, at most once in the life of the process.
///
/// # Why once, and why that is not obvious
///
/// Because a host runs TWO network instances, the room and the lobby, and this
/// is called from the code that starts either. `log::init` ends in
/// `try_init`, which fails on the second call: a per-instance call would work
/// for the room and quietly error for the lobby.
///
/// # Why nothing happens without a directory
///
/// Running `kanpachi-engine.exe` by hand is a supported thing —it starts an
/// empty instance that knows no room— and it should not scatter log files in
/// whatever directory somebody happened to be in. No directory, no subscriber,
/// which is exactly the behaviour this binary had until now.
pub fn init_once(dir: Option<&str>) {
    let Some(dir) = dir else { return };
    if dir.is_empty() {
        return;
    }

    UNA_VEZ.call_once(|| {
        let cfg = LoggingConfig {
            file_logger: Some(FileLoggerConfig {
                level: Some("info".to_string()),
                file: Some(FILE.to_string()),
                dir: Some(dir.to_string()),
                size_mb: Some(SIZE_MB),
                count: Some(COUNT),
            }),
            // OFF, y escrito. Ver el doc del módulo: encenderlo manda INFO a
            // stdout, que es el canal de órdenes.
            console_logger: Some(ConsoleLoggerConfig {
                level: Some("off".to_string()),
            }),
        };

        // El error se traga, y es la decisión correcta: quedarse sin log es
        // perder diagnóstico, y negarse a abrir la sala por eso sería cambiar
        // un problema de diagnóstico por uno de producto. Va por `eprintln`
        // porque todavía no hay log al que contarlo, y stderr sí lo recoge el
        // daemon.
        if let Err(e) = easytier::common::log::init(&cfg, false) {
            eprintln!("kanpachi-engine: could not start logging in {dir}: {e}");
        }
    });
}
