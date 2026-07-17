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
use crate::event::SessionPhase;
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
