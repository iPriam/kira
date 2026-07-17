//! The runner's half of a live session: fetch the bundle, load it, report.
//!
//! [`RunnerClient`] owns the protocol; [`RunnerHost`] owns what a bundle *means*
//! on a given platform. That split is the point. This crate must not know how a
//! desktop runner loads bytecode or how an Apple runner links a signed app —
//! each runner implements the trait and this drives it. So a new runner adds a
//! `RunnerHost` and nothing here changes.
//!
//! The client reports each milestone only after the host actually reached it.
//! [`RunnerClient::run_session`] calls `load`, and only if `load` returns `Ok`
//! does it report `BundleLoaded`. A host that fails reports `Failed` with its
//! reason and the session ends — it never falls through to the next milestone.

use std::fmt;
use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use kira_manifest::RunnerId;

use crate::bundle::BundleManifest;
use crate::event::{ReloadMode, SessionPhase};
use crate::protocol::{
    ClientMessage, PROTOCOL_VERSION, ProtocolError, ServerMessage, read_message, write_message,
};
use crate::store::{Bundle, BundleError};

/// How long a runner waits on a server that has gone quiet.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a runner waits for the server to accept bytes.
///
/// Bounded for the same reason as the read: a server that stops reading would
/// otherwise block this runner in `write_all` forever once the send buffer
/// fills, and a runner that cannot be killed by its own timeout is a runner that
/// outlives the session that started it.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a platform actually does with a bundle.
///
/// Implemented once per runner. The three steps are separate because they fail
/// for different reasons and a session needs to say which one failed: a bundle
/// that will not load is a different problem from one that loads and will not
/// link.
pub trait RunnerHost {
    /// Why this host could not do something.
    ///
    /// An associated type because this crate cannot enumerate the failures of
    /// runners it does not know about. It crosses the wire as its `Display`
    /// text, which is the only form the other end could use anyway.
    type Error: fmt::Display;

    /// Loads the bundle's payloads into the process.
    fn load(&mut self, bundle: &Bundle) -> Result<(), Self::Error>;

    /// Links what was loaded, resolving whatever the payloads need from each
    /// other and from the host.
    fn link(&mut self) -> Result<(), Self::Error>;

    /// Starts the app's entrypoint.
    ///
    /// Returns when the entrypoint has started — which for a windowed runner is
    /// not when it has finished.
    fn start(&mut self) -> Result<(), Self::Error>;

    /// Swaps `bundle` into the running process, in place.
    ///
    /// The supervisor has already established that the swap is possible — the
    /// native half is byte-identical, so the process's loaded code is still
    /// current — and this is the host committing to it. The process, its loaded
    /// libraries, and anything they hold survive; the bytecode does not.
    ///
    /// A host that cannot take a particular swap returns an error, and the
    /// session relaunches instead. That is the honest answer and it is always
    /// available: only the host knows what its own live values depend on.
    fn swap(&mut self, bundle: &Bundle) -> Result<(), Self::Error>;

    /// Whether this host refuses hot patching outright.
    ///
    /// A host with the kill switch set answers `true` and every reload
    /// relaunches, which is what makes it possible to tell whether a bug belongs
    /// to the hot-patch path or was always there. Defaults to `false`: a host
    /// that does not care need not say so.
    fn hotpatch_disabled(&self) -> bool {
        false
    }
}

/// An error running a live session from the runner's side.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket failed.
    #[error("live client socket failed: {0}")]
    Io(#[from] std::io::Error),
    /// The protocol failed.
    #[error("live client protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    /// The server's first message was not a `Welcome`.
    #[error("live server did not welcome this runner")]
    NoWelcome,
    /// The server speaks a protocol version this build does not.
    #[error("live server speaks protocol {theirs}, this runner speaks {ours}")]
    VersionMismatch {
        /// The version the server announced.
        theirs: u16,
        /// The version this build speaks.
        ours: u16,
    },
    /// The server sent something other than what was asked for.
    #[error("live server sent an unexpected message while {while_doing}")]
    Unexpected {
        /// What the runner was doing when the message arrived.
        while_doing: &'static str,
    },
    /// The server had no payload its own manifest names.
    #[error("live server has no payload `{name}`, which its manifest names")]
    MissingPayload {
        /// The payload's name.
        name: String,
    },
    /// The bundle did not survive verification against its manifest.
    #[error("live bundle did not verify: {0}")]
    Bundle(#[from] BundleError),
    /// The bundle's manifest did not decode.
    #[error("live bundle manifest did not decode: {0}")]
    Manifest(#[from] crate::bundle::BundleDecodeError),
    /// The server ended the session.
    #[error("live server shut the session down")]
    ShutDown,
    /// The host could not load, link, or start the bundle.
    #[error("runner could not {step} the bundle: {reason}")]
    Host {
        /// Which step failed.
        step: &'static str,
        /// What the host said about it.
        reason: String,
    },
}

/// A runner's connection to a live server.
#[derive(Debug)]
pub struct RunnerClient {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    /// The bundle the runner currently holds, kept so a reload can reuse the
    /// payloads that did not change rather than re-downloading them.
    loaded: Option<Bundle>,
}

impl RunnerClient {
    /// Connects to a live server and completes the handshake.
    ///
    /// Returns once the server has welcomed this runner, so a returned client is
    /// one the server has agreed to serve.
    pub fn connect(address: SocketAddr, runner: RunnerId) -> Result<RunnerClient, ClientError> {
        let stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        stream.set_nodelay(true)?;
        let mut client = RunnerClient {
            reader: BufReader::new(stream.try_clone()?),
            writer: BufWriter::new(stream),
            loaded: None,
        };
        client.handshake(runner)?;
        Ok(client)
    }

    /// Sends `Hello` and waits for `Welcome`.
    fn handshake(&mut self, runner: RunnerId) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::Hello {
                protocol: PROTOCOL_VERSION,
                runner,
            },
        )?;
        match read_message(&mut self.reader)? {
            ServerMessage::Welcome { protocol } if protocol == PROTOCOL_VERSION => Ok(()),
            ServerMessage::Welcome { protocol } => Err(ClientError::VersionMismatch {
                theirs: protocol,
                ours: PROTOCOL_VERSION,
            }),
            ServerMessage::Shutdown => Err(ClientError::ShutDown),
            _ => Err(ClientError::NoWelcome),
        }
    }

    /// Downloads the bundle: its manifest, then every payload the manifest names.
    ///
    /// The returned bundle has been verified against its manifest — every
    /// payload hashed and checked — so a caller holds bytes that are what the
    /// build produced or holds an error, never something in between.
    pub fn fetch_bundle(&mut self) -> Result<Bundle, ClientError> {
        write_message(&mut self.writer, &ClientMessage::RequestBundle)?;
        let manifest = match read_message(&mut self.reader)? {
            ServerMessage::Manifest { bytes } => BundleManifest::from_bytes(&bytes)?,
            ServerMessage::Shutdown => return Err(ClientError::ShutDown),
            _ => {
                return Err(ClientError::Unexpected {
                    while_doing: "asking for the bundle manifest",
                });
            }
        };
        let bundle = self.download(manifest)?;
        // Remembered so a later reload can reuse what did not change.
        self.loaded = Some(bundle.clone());
        Ok(bundle)
    }

    /// Reports that a milestone actually happened.
    pub fn report(&mut self, phase: SessionPhase) -> Result<(), ClientError> {
        write_message(&mut self.writer, &ClientMessage::Progress { phase })?;
        Ok(())
    }

    /// Reports that this runner could not continue, and why.
    pub fn fail(&mut self, reason: &str) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::Failed {
                reason: reason.to_owned(),
            },
        )?;
        Ok(())
    }

    /// Ends the session cleanly.
    pub fn goodbye(&mut self) -> Result<(), ClientError> {
        write_message(&mut self.writer, &ClientMessage::Goodbye)?;
        Ok(())
    }

    /// Runs a whole session: download the bundle, drive `host` through it, and
    /// report each milestone the host actually reaches.
    ///
    /// A host failure is reported to the server before it is returned, so the
    /// server's session ends with the runner's reason rather than with a bare
    /// disconnect it would have to guess about.
    pub fn run_session<H: RunnerHost>(&mut self, host: &mut H) -> Result<Bundle, ClientError> {
        let bundle = self.fetch_bundle()?;
        self.report(SessionPhase::BundleReceived)?;

        self.step("load", host.load(&bundle))?;
        self.report(SessionPhase::BundleLoaded)?;

        self.step("link", host.link())?;
        self.report(SessionPhase::BundleLinked)?;

        self.step("start", host.start())?;
        self.report(SessionPhase::EntrypointStarted)?;

        Ok(bundle)
    }

    /// Stays connected, taking reloads until the server ends the session.
    ///
    /// This is what makes a runner a runner rather than a one-shot: the app is
    /// up, and the process waits to be told that a rebuilt bundle exists.
    ///
    /// Each reload is downloaded, staged, swapped, and run — and each of those
    /// four is reported only after it actually happened. `ReloadApplied` goes out
    /// after the swap returns; `ReloadCompleted` only after the new code has run
    /// once without incident, which is why the two are separate. A swap that
    /// commits and then traps on its first call is not a reload that worked, and
    /// the session must be able to tell the difference.
    pub fn serve_reloads<H: RunnerHost>(&mut self, host: &mut H) -> Result<(), ClientError> {
        loop {
            // Waiting for a save is unbounded, and must be. A read timeout here
            // would make the runner kill itself for the crime of the developer
            // thinking for half a minute — the app would be gone by the time they
            // saved, and every reload after an idle gap would silently degrade to
            // a relaunch. Nothing is lost by waiting forever: a server that dies
            // closes the socket, and a closed socket is a read of zero bytes, not
            // a hang. That is the `Disconnected` arm below.
            self.set_read_timeout(None)?;
            let message: ServerMessage = match read_message(&mut self.reader) {
                Ok(message) => message,
                // The server went away without saying goodbye. The session is
                // over either way; a runner outliving its server is an orphan.
                Err(ProtocolError::Disconnected) => return Ok(()),
                Err(error) => return Err(error.into()),
            };

            // Bounded again for everything that follows: once the server has
            // spoken, the rest of the exchange is a conversation it is driving,
            // and a peer that stops mid-download must not wedge this process.
            self.set_read_timeout(Some(READ_TIMEOUT))?;

            match message {
                ServerMessage::Shutdown => return Ok(()),
                ServerMessage::Reload { mode, manifest } => {
                    self.apply_reload(host, mode, &manifest)?;
                }
                _ => {
                    return Err(ClientError::Unexpected {
                        while_doing: "waiting for a reload",
                    });
                }
            }
        }
    }

    /// Sets how long a read waits, or `None` to wait indefinitely.
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<(), ClientError> {
        self.reader.get_ref().set_read_timeout(timeout)?;
        Ok(())
    }

    /// Takes one reload, reporting each step as it actually happens.
    fn apply_reload<H: RunnerHost>(
        &mut self,
        host: &mut H,
        mode: ReloadMode,
        manifest: &[u8],
    ) -> Result<(), ClientError> {
        // A relaunch is not something a runner does to itself: the supervisor
        // replaces the process. Being asked to relaunch in place is a server that
        // has confused the two tiers, and doing nothing quietly would leave it
        // waiting forever.
        if mode == ReloadMode::Relaunch {
            self.restart_required("a runner cannot relaunch itself in place")?;
            return Ok(());
        }
        if host.hotpatch_disabled() {
            self.restart_required(&format!(
                "hot patching is disabled for this runner ({}=1)",
                crate::reload::NO_HOTPATCH_VAR
            ))?;
            return Ok(());
        }

        let manifest = BundleManifest::from_bytes(manifest)?;
        // Only what actually changed comes over the wire. A hot patch's whole
        // premise is that the native library is byte-identical, so re-sending it
        // would mean shipping megabytes to prove they are the same megabytes —
        // the payload hashes exist exactly so that nobody has to.
        let bundle = match self.download_reusing(manifest) {
            Ok(bundle) => bundle,
            // A bundle that will not download is not a bundle to swap to, and
            // the running app is still fine — so this is a restart requirement,
            // not a session failure.
            Err(error) => {
                self.restart_required(&format!("the rebuilt bundle did not arrive: {error}"))?;
                return Ok(());
            }
        };

        write_message(&mut self.writer, &ClientMessage::ReloadStaged)?;

        if let Err(error) = host.swap(&bundle) {
            let reason = error.to_string();
            write_message(&mut self.writer, &ClientMessage::ReloadRejected { reason })?;
            return Ok(());
        }
        write_message(&mut self.writer, &ClientMessage::ReloadApplied)?;

        // The swap is committed; now prove it runs. A trap here is a rejection
        // of the reload rather than a failure of the session: the supervisor
        // relaunches, and the developer sees the trap.
        if let Err(error) = host.start() {
            let reason = format!("the swapped-in code did not run: {error}");
            write_message(&mut self.writer, &ClientMessage::ReloadRejected { reason })?;
            return Ok(());
        }
        write_message(&mut self.writer, &ClientMessage::ReloadCompleted)?;
        Ok(())
    }

    /// Tells the server this runner must be replaced, and why.
    fn restart_required(&mut self, reason: &str) -> Result<(), ClientError> {
        write_message(
            &mut self.writer,
            &ClientMessage::RestartRequired {
                reason: reason.to_owned(),
            },
        )?;
        Ok(())
    }

    /// Downloads `manifest`'s payloads, reusing any the runner already holds.
    ///
    /// A payload is reused only when its hash matches, and the reused bytes go
    /// through the same verification as downloaded ones — so a reused payload is
    /// exactly the payload the manifest names, or the bundle is refused.
    fn download_reusing(&mut self, manifest: BundleManifest) -> Result<Bundle, ClientError> {
        let held = self.loaded.take();
        let mut payloads = Vec::with_capacity(manifest.payloads.len());
        for entry in &manifest.payloads {
            let reused = held.as_ref().and_then(|bundle| {
                let previous = bundle.manifest().payload(&entry.name)?;
                (previous.hash == entry.hash)
                    .then(|| bundle.payload_by_name(&entry.name))
                    .flatten()
                    .map(<[u8]>::to_vec)
            });
            if let Some(bytes) = reused {
                payloads.push(Some(bytes));
                continue;
            }

            write_message(
                &mut self.writer,
                &ClientMessage::RequestPayload {
                    name: entry.name.clone(),
                },
            )?;
            payloads.push(Some(self.receive_payload(&entry.name)?));
        }
        let bundle = Bundle::assemble(manifest, payloads)?;
        self.loaded = Some(bundle.clone());
        Ok(bundle)
    }

    /// Reads one payload the server was asked for by `name`.
    fn receive_payload(&mut self, name: &str) -> Result<Vec<u8>, ClientError> {
        match read_message(&mut self.reader)? {
            ServerMessage::Payload { name: got, bytes } if got == name => Ok(bytes),
            // A payload arriving under a name that was not asked for would
            // desync the manifest from the bytes, so it is refused rather than
            // stored at whatever index happens to be next.
            ServerMessage::Payload { .. } => Err(ClientError::Unexpected {
                while_doing: "downloading payloads",
            }),
            ServerMessage::NoSuchPayload { name } => Err(ClientError::MissingPayload { name }),
            ServerMessage::Shutdown => Err(ClientError::ShutDown),
            _ => Err(ClientError::Unexpected {
                while_doing: "downloading payloads",
            }),
        }
    }

    /// Downloads every payload `manifest` names and verifies them against it.
    fn download(&mut self, manifest: BundleManifest) -> Result<Bundle, ClientError> {
        let mut payloads = Vec::with_capacity(manifest.payloads.len());
        for entry in &manifest.payloads {
            write_message(
                &mut self.writer,
                &ClientMessage::RequestPayload {
                    name: entry.name.clone(),
                },
            )?;
            match read_message(&mut self.reader)? {
                ServerMessage::Payload { name, bytes } if name == entry.name => {
                    payloads.push(Some(bytes));
                }
                // A payload arriving under a name that was not asked for would
                // desync the manifest from the bytes, so it is refused rather
                // than stored at whatever index happens to be next.
                ServerMessage::Payload { .. } => {
                    return Err(ClientError::Unexpected {
                        while_doing: "downloading payloads",
                    });
                }
                ServerMessage::NoSuchPayload { name } => {
                    return Err(ClientError::MissingPayload { name });
                }
                ServerMessage::Shutdown => return Err(ClientError::ShutDown),
                _ => {
                    return Err(ClientError::Unexpected {
                        while_doing: "downloading payloads",
                    });
                }
            }
        }
        Ok(Bundle::assemble(manifest, payloads)?)
    }

    /// Maps a host step's result, telling the server about a failure first.
    fn step<E: fmt::Display>(
        &mut self,
        step: &'static str,
        result: Result<(), E>,
    ) -> Result<(), ClientError> {
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = error.to_string();
                // Best-effort: the session is already failing, and a send that
                // also fails must not replace the host's reason with a socket
                // error that says nothing about why the app did not run.
                let _ = self.fail(&format!("{step}: {reason}"));
                Err(ClientError::Host { step, reason })
            }
        }
    }
}
