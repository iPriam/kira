//! A hybrid library's two halves, and what has to agree before either runs.
//!
//! # Three artifacts, two of them embedded
//!
//! A hybrid library build writes a `.kbc`, a `.khm`, and a shared library. The
//! first two are **data**, so the generated wrapper `include_bytes!`s both and
//! the crate stays relocatable exactly as the VM engine's does. The third is
//! code the process must `dlopen`, so it stays a file and is found at load time
//! ([`crate::locate`]).
//!
//! That split is deliberate rather than incidental: it makes the deployment
//! story exactly one file long. A consumer ships their binary plus
//! `lib<name>.dylib`, and everything else about the library is already inside
//! the binary.
//!
//! # What is checked, in what order
//!
//! 1. The bytecode half decodes and validates, and matches the contract the
//!    wrapper was generated against ([`kira_main::Library::verify`]).
//! 2. The manifest decodes.
//! 3. The two halves describe the same program — same function count, same
//!    arities, same engine per function. This is the check that catches a `.kbc`
//!    from one build sitting beside a `.khm` from another, and it is the hybrid
//!    engine's own stale-build guard.
//! 4. Only then is the shared library found and opened.
//!
//! Ordered cheapest-and-most-specific first: a mismatch that can be reported
//! from two byte arrays should never require finding a file on disk to discover.

use std::rc::Rc;

use kira_hybrid_definition::HybridManifest;
use kira_hybrid_runtime::NativeLibrary;
use kira_main::{ExportContract, StdoutHost};
use kira_runtime_abi::HostCapabilities;

use crate::error::HybridMainError;
use crate::instance::HybridInstance;
use crate::locate;

/// A hybrid Kira library: both halves described, neither running.
///
/// Holds the decoded bytecode half and the manifest. The shared library is
/// *not* opened here — that happens per instance, so two instances of one
/// library get two independent heaps the way they do on the VM engine.
#[derive(Debug, Clone)]
pub struct HybridLibrary {
    name: String,
    bytecode: kira_main::Library,
    manifest: HybridManifest,
}

impl HybridLibrary {
    /// Decodes both halves' descriptions and proves they agree.
    ///
    /// `name` is the package name, which is what the native half is looked up
    /// by and what every message names.
    pub fn from_parts(
        name: &str,
        bytecode: &[u8],
        manifest: &[u8],
    ) -> Result<HybridLibrary, HybridMainError> {
        let bytecode = kira_main::Library::from_bytes(bytecode)?;
        let manifest = HybridManifest::from_bytes(manifest)?;
        // Reused rather than restated: the application-side session validates a
        // bundle with exactly this function, and two answers to "do these halves
        // agree" is one too many.
        kira_hybrid_runtime::validate::bundle(&manifest, bytecode.module()).map_err(|source| {
            HybridMainError::Mismatch {
                library: name.to_owned(),
                reason: source.to_string(),
            }
        })?;
        Ok(HybridLibrary {
            name: name.to_owned(),
            bytecode,
            manifest,
        })
    }

    /// The library's package name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The manifest describing which engine owns each function.
    pub fn manifest(&self) -> &HybridManifest {
        &self.manifest
    }

    /// The bytecode half, whose export table is the consumer-facing surface.
    pub fn bytecode(&self) -> &kira_main::Library {
        &self.bytecode
    }

    /// Checks that this library is the one `contract` was generated from.
    ///
    /// The bytecode half's check, unchanged — it covers the export surface, and
    /// the export surface is the whole of what a consumer can call. The native
    /// half is guarded separately by the halves-agree check in
    /// [`from_parts`](HybridLibrary::from_parts), which is stricter than a
    /// symbol marker would be: it compares every function's engine and arity
    /// rather than one name.
    pub fn verify(&self, contract: &ExportContract<'_>) -> Result<(), HybridMainError> {
        Ok(self.bytecode.verify(contract)?)
    }

    /// Instantiates with the default [`StdoutHost`], finding the native half.
    ///
    /// `baked` is the absolute path the build wrote the shared library to, and
    /// is the *last* place looked; see [`crate::locate`].
    pub fn instantiate(
        &self,
        baked: &std::path::Path,
    ) -> Result<HybridInstance<StdoutHost>, HybridMainError> {
        self.instantiate_with(StdoutHost, baked)
    }

    /// Instantiates with a host the embedder supplies, finding the native half.
    pub fn instantiate_with<H: HostCapabilities>(
        &self,
        host: H,
        baked: &std::path::Path,
    ) -> Result<HybridInstance<H>, HybridMainError> {
        let path = locate::find(&self.name, baked).map_err(|tried| {
            HybridMainError::NativeHalfMissing {
                library: self.name.clone(),
                variable: locate::override_variable(&self.name),
                tried,
            }
        })?;
        // A consumed hybrid library binds no callback thunks: it installs no
        // runtime invoker either (see below), so nothing could enter Kira
        // through one.
        let native =
            NativeLibrary::load(Some(&path), &self.manifest.functions, 0).map_err(|source| {
                HybridMainError::NativeHalf {
                    library: self.name.clone(),
                    source: Box::new(source),
                }
            })?;
        // No runtime invoker is installed. See `instance.rs`: a library instance
        // owns a heap and calls through `&mut self`, so a native function
        // calling back into it would need a second mutable borrow of the same
        // instance. The build refuses that program by name; this is the half
        // that makes sure it cannot happen anyway.
        HybridInstance::new(&self.bytecode, host, Rc::new(native))
    }
}
