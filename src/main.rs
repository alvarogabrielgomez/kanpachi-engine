//! Kanpachi's network engine.
//!
//! Reads commands on stdin, writes answers and events on stdout, and has no
//! other way in.
//!
//! # Why this binary exists at all
//!
//! `easytier-core.exe` opens an administration portal with **no authentication
//! of any kind**, and its default binds every interface. Through it any local
//! process issues credentials for the real network, adds nodes, forwards ports
//! underneath the firewall calculation, and reads the network secret in the
//! clear. Authentication was asked for upstream and refused on purpose, in
//! favour of an IP whitelist that filters after accept and whose default
//! already covers loopback, so it cannot tell one local process from another.
//!
//! Consuming EasyTier as a library removes the portal by omission rather than
//! by configuration: `ApiRpcServer::new` is constructed in exactly one place in
//! the whole tree, inside that command-line binary. There is no flag to forget.
//!
//! # The one command channel
//!
//! The daemon creates this process and talks to it through the pipes it made in
//! the same act. Those pipes are **anonymous**: no name, no path, no address.
//! It is not that connecting to them is forbidden, it is that the operation
//! does not exist. The only two ends live as handles inside the daemon and this
//! process.
//!
//! **stdin is the only way to command this engine.** Never a port, never a
//! named pipe, never a watched file, never a signal. Running this executable by
//! hand starts a second, empty instance that knows no room's secret and touches
//! no tunnel of the daemon's.

mod config;
mod engine;
mod log;
mod proto;

use std::io::IsTerminal;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::engine::Engine;
use crate::proto::{Command, Data, Outgoing, RenewedOut, Request, Response};

/// The cap on one incoming line.
///
/// Framing by line means an oversized message leaves the stream out of step:
/// what follows is the tail of something never read whole, and carrying on
/// would be reading rubbish as commands. So the cap is not a courtesy, it is
/// the point at which this process stops reading. The daemon's own transports
/// use the same discipline.
const MAX_LINE: usize = 1 << 20;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // No arguments, ever. Every knob this engine has arrives over stdin from
    // the process that created it, so an argument can only be somebody trying
    // to steer it from outside that channel.
    if std::env::args_os().count() > 1 {
        eprintln!(
            "kanpachi-engine takes no arguments. It is driven over stdin by the Kanpachi \
             daemon, which is the only command channel it has."
        );
        return std::process::ExitCode::from(2);
    }

    // Not a refusal, a warning. Somebody running this by hand gets an empty
    // instance that knows nothing, which is harmless and confusing, so say so.
    if std::io::stdin().is_terminal() {
        eprintln!(
            "kanpachi-engine is reading commands from a terminal. This instance is empty: it \
             knows no room and touches nothing the Kanpachi daemon is running."
        );
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();

    // One writer and one only. Responses and events share stdout, and two
    // tasks writing to it would interleave two JSON objects on one line.
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = rx.recv().await {
            let mut line = match serde_json::to_vec(&msg) {
                Ok(l) => l,
                // Unserialisable output is a bug here, not input we were given.
                // Killing the channel over it would take the room down with it.
                Err(e) => {
                    eprintln!("kanpachi-engine: could not serialise an outgoing message: {e}");
                    continue;
                }
            };
            line.push(b'\n');
            if stdout.write_all(&line).await.is_err() || stdout.flush().await.is_err() {
                // The daemon is gone. Nothing left to answer.
                return;
            }
        }
    });

    let mut eng = Engine::new(tx.clone());
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            // Stdin closed: the daemon exited, or the pipe broke. Either way
            // this engine has nobody to serve.
            Ok(None) => break,
            Err(e) => {
                eprintln!("kanpachi-engine: reading stdin failed: {e}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE {
            eprintln!(
                "kanpachi-engine: a command of {} bytes went past the {MAX_LINE} byte cap. \
                 The stream is out of step, so reading stops here.",
                line.len()
            );
            break;
        }

        // Strict on purpose, both ways: an unknown field rejects the whole
        // message and trailing content does too. A field too many is not
        // somebody being generous with extensions, it is somebody who believes
        // they are asking for something this engine does not do.
        let req: Request = match strict(&line) {
            Ok(r) => r,
            Err(e) => {
                // A rejected message still has to be ANSWERED when its id can
                // be recovered. The first version of this dropped it and only
                // logged, and the smoke test showed what that costs: the
                // daemon sends a malformed command and waits for a reply that
                // never comes. Silence on the engine's side turns a bug in the
                // caller into a hang.
                //
                // Only the id is read back, with the loosest possible parse, so
                // that recovering it cannot itself trip over whatever made the
                // message unreadable.
                match serde_json::from_str::<JustId>(&line) {
                    Ok(j) => {
                        let _ = tx.send(Outgoing::Response(Response::failed(
                            j.id,
                            format!("unreadable command: {e}"),
                        )));
                    }
                    Err(_) => {
                        // No id anywhere: there is nothing to answer to.
                        eprintln!("kanpachi-engine: unreadable command with no id: {e}");
                    }
                }
                continue;
            }
        };

        let reply = dispatch(&mut eng, &req).await;
        if tx.send(Outgoing::Response(reply)).is_err() {
            break;
        }
    }

    // Shutdown, in this order, and the order is the whole of it.
    //
    // # The bug this shape exists to prevent, which was measured and not
    // imagined
    //
    // The first version did `eng.leave()`, then `drop(tx)`, then
    // `writer.await`, and **the process never exited**. Two engines were still
    // alive minutes later with fifteen milliseconds of CPU between them.
    //
    // The reason is that `tx` was not the only sender. `Engine` holds a clone,
    // and so does every event pump task. Dropping the one named `tx` leaves the
    // channel open, `rx.recv()` never yields `None`, and the writer task waits
    // for a message that will never come while the main task waits for the
    // writer. A closed stdin left a process holding a virtual network up.
    //
    // So: bring the networks down, then drop the engine so its sender and its
    // instances go with it, which closes the event bus and ends the pumps, and
    // only then drop ours. The timeout is the backstop, because a shutdown path
    // that can hang is the same bug with a longer fuse.
    eng.leave();
    drop(eng);
    tokio::time::sleep(engine::SHUTDOWN_GRACE).await;
    drop(tx);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), writer).await;
    std::process::ExitCode::SUCCESS
}

/// Just enough of a message to answer it when the rest of it is unreadable.
///
/// Deliberately NOT strict: this parse runs after the strict one already
/// failed, and its only job is to recover the id so the caller gets an error
/// instead of silence.
#[derive(Deserialize)]
struct JustId {
    id: u64,
}

/// Decodes rejecting unknown fields and trailing content.
fn strict(line: &str) -> anyhow::Result<Request> {
    let mut de = serde_json::Deserializer::from_str(line);
    let req = Request::deserialize(&mut de)?;
    de.end()?;
    Ok(req)
}

async fn dispatch(eng: &mut Engine, req: &Request) -> Response {
    let id = req.id;
    match &req.cmd {
        Command::Host(a) => match eng.host(a).await {
            Ok(()) => Response::ok(id),
            Err(e) => Response::failed(id, e),
        },
        Command::JoinRendezvous(a) => match eng.join_rendezvous(a).await {
            Ok(()) => Response::ok(id),
            Err(e) => Response::failed(id, e),
        },
        Command::LeaveRendezvous(_) => {
            eng.leave_rendezvous();
            Response::ok(id)
        }
        Command::Join(a) => match eng.join(a).await {
            Ok(()) => Response::ok(id),
            Err(e) => Response::failed(id, e),
        },
        Command::Leave(_) => {
            eng.leave();
            Response::ok(id)
        }
        Command::IssueCredential(a) => match eng.issue_credential(a).await {
            Ok(c) => Response::with(id, Data::Credential(c)),
            Err(e) => Response::failed(id, e),
        },
        Command::RenewCredential(a) => match eng.renew_credential(a).await {
            Ok(expiry_unix) => Response::with(id, Data::Renewed(RenewedOut { expiry_unix })),
            Err(e) => Response::failed(id, e),
        },
        Command::RevokeCredential(a) => match eng.revoke_credential(a).await {
            Ok(()) => Response::ok(id),
            Err(e) => Response::failed(id, e),
        },
        Command::ListCredentials(_) => match eng.list_credentials().await {
            Ok(c) => Response::with(id, Data::Credentials(c)),
            Err(e) => Response::failed(id, e),
        },
        Command::Peers(_) => match eng.peers().await {
            Ok(p) => Response::with(id, Data::Peers(p)),
            Err(e) => Response::failed(id, e),
        },
        Command::Diagnostics(_) => match eng.diagnostics().await {
            Ok(d) => Response::with(id, Data::Diagnostics(d)),
            Err(e) => Response::failed(id, e),
        },
    }
}
