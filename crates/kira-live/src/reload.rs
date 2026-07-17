//! Deciding how a rebuilt bundle reaches a running app.
//!
//! Two tiers. A **hot patch** swaps the bytecode into the process that is
//! already running: same process, same loaded native library, and — once the
//! language has state that outlives a call — the same heap. A **relaunch** kills
//! the runner and starts a new one, which loses everything the process held.
//!
//! The tier is chosen by what actually changed, and the rule is byte identity
//! rather than a source diff: if the rebuilt native library is byte-for-byte the
//! loaded one, the edit was a bytecode-only edit whatever the source looked
//! like, and the running process can take it. If it differs by one byte, it
//! cannot — the process has that library's code and layouts baked into it, and
//! pointers into it held by native state. This is why bundle payloads are named
//! by a collision-resistant hash: a hash collision here would hot-patch across
//! an ABI change and corrupt memory silently.
//!
//! Nothing degrades quietly. A bundle that cannot be hot-patched produces a
//! [`ReloadDecision`] that says so and why, and the reason reaches the user —
//! rather than a relaunch that looks like a slow hot patch and leaves someone
//! wondering where their state went.

use crate::bundle::{BundleManifest, PayloadEntry};
use crate::event::ReloadMode;

/// The environment variable that turns tier 1 off.
///
/// Set it to `1` and every reload relaunches. It exists so that a session can be
/// run with the hot-patch path removed rather than merely unused — which is what
/// makes it possible to tell whether a bug is the hot patch's fault.
pub const NO_HOTPATCH_VAR: &str = "KIRA_LIVE_NO_HOTPATCH";

/// Whether the environment asks for relaunch-only reloads.
///
/// Read where a session starts rather than per reload: an env var that can
/// change under a running session is a session that behaves two ways for one
/// invocation.
pub fn hotpatch_disabled_by_env() -> bool {
    std::env::var(NO_HOTPATCH_VAR).is_ok_and(|value| value == "1")
}

/// Why a rebuilt bundle cannot be swapped into the running process.
///
/// Each variant is a fact about the two bundles, not a guess. The message a user
/// sees comes from here, so "restart required" always arrives with the reason it
/// was required.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelaunchReason {
    /// The native library changed, so the loaded one is stale.
    ///
    /// The running process cannot take this: its code is mapped, and native
    /// state holds pointers into it. Swapping the bytecode alone would leave the
    /// two halves disagreeing about what the other one is.
    #[error("the native library `{payload}` changed, and a loaded library cannot be swapped")]
    NativeLibraryChanged {
        /// Which payload changed.
        payload: String,
    },
    /// The hybrid manifest changed, so the VM/native boundary moved.
    ///
    /// A `KHM1` manifest records which engine owns each function and what its
    /// signature is. A change there means a call that used to cross the boundary
    /// one way now crosses it another, which the loaded native half was compiled
    /// against and cannot be told about.
    #[error("the hybrid manifest `{payload}` changed, so the native boundary moved")]
    HybridManifestChanged {
        /// Which payload changed.
        payload: String,
    },
    /// A payload that is not hot-swappable changed.
    #[error("the `{kind}` payload `{payload}` changed, and it cannot be swapped in place")]
    PayloadChanged {
        /// Which payload changed.
        payload: String,
        /// What kind it is.
        kind: &'static str,
    },
    /// The bundle's payloads are not the same set they were.
    ///
    /// A payload appearing or disappearing is a different program shape, not an
    /// edit to the one that is loaded.
    #[error("the bundle's payloads changed: {detail}")]
    PayloadSetChanged {
        /// What differs.
        detail: String,
    },
    /// The bundle is for a different runner or profile than the loaded one.
    #[error("the rebuilt bundle is for a different {field} than the running one")]
    BundleIdentityChanged {
        /// Which field differs.
        field: &'static str,
    },
    /// The entrypoint payload is not one that can be swapped.
    #[error("the entrypoint is a `{kind}` payload, which cannot be swapped in place")]
    EntryNotSwappable {
        /// The entry's kind.
        kind: &'static str,
    },
    /// Tier 1 was turned off for this session.
    #[error("hot patching is disabled for this session ({NO_HOTPATCH_VAR}=1)")]
    DisabledByEnv,
    /// The runner refused the hot patch.
    ///
    /// The supervisor's comparison says a swap is possible; only the runner
    /// knows whether its own live values can survive one, and it gets the last
    /// word.
    #[error("the runner could not apply the hot patch: {reason}")]
    RunnerRefused {
        /// What the runner said.
        reason: String,
    },
}

/// What to do with a rebuilt bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadDecision {
    /// Nothing changed; there is nothing to do.
    ///
    /// A real outcome rather than a degenerate one: a save that changed no bytes
    /// must not restart anybody's app.
    Unchanged,
    /// Swap the bytecode into the running process.
    HotPatch,
    /// Kill the runner and start a new one, for this reason.
    Relaunch {
        /// Why a swap was not possible.
        reason: RelaunchReason,
    },
}

impl ReloadDecision {
    /// The mode this decision applies, or `None` when there is nothing to apply.
    pub fn mode(&self) -> Option<ReloadMode> {
        match self {
            Self::Unchanged => None,
            Self::HotPatch => Some(ReloadMode::HotPatch),
            Self::Relaunch { .. } => Some(ReloadMode::Relaunch),
        }
    }
}

/// Decides how `rebuilt` reaches a process running `loaded`.
///
/// The comparison is between two manifests, because a manifest is a list of
/// content hashes: deciding from hashes rather than from bytes means the
/// decision costs nothing and does not depend on still having the old payloads
/// around.
///
/// # What is not checked yet
///
/// The specification names two more rejection conditions: a struct or enum whose
/// layout changed, and a function referenced by a live closure that was removed
/// or had its signature changed. Neither is checked here, because neither can
/// happen yet — the language has no structs, no enums, and no closures, so there
/// are no layouts to change and no live values holding function ids across a
/// swap. They are not skipped; there is nothing to skip. When those features
/// land, this is the function that grows the checks, and the hot patch must not
/// be trusted for them until it does.
pub fn decide(
    loaded: &BundleManifest,
    rebuilt: &BundleManifest,
    hotpatch_disabled: bool,
) -> ReloadDecision {
    if manifests_match(loaded, rebuilt) {
        return ReloadDecision::Unchanged;
    }
    if hotpatch_disabled {
        return ReloadDecision::Relaunch {
            reason: RelaunchReason::DisabledByEnv,
        };
    }
    if let Some(reason) = swap_blocker(loaded, rebuilt) {
        return ReloadDecision::Relaunch { reason };
    }
    ReloadDecision::HotPatch
}

/// Whether the two manifests describe byte-for-byte the same bundle.
fn manifests_match(loaded: &BundleManifest, rebuilt: &BundleManifest) -> bool {
    loaded == rebuilt
}

/// The first reason `rebuilt` cannot be swapped into a process running `loaded`,
/// or `None` if it can.
fn swap_blocker(loaded: &BundleManifest, rebuilt: &BundleManifest) -> Option<RelaunchReason> {
    if loaded.runner != rebuilt.runner {
        return Some(RelaunchReason::BundleIdentityChanged { field: "runner" });
    }
    if loaded.profile != rebuilt.profile {
        return Some(RelaunchReason::BundleIdentityChanged { field: "profile" });
    }

    // A payload appearing, disappearing, or changing kind is a different program
    // shape. Checked before the per-payload compare so the reason names the real
    // difference rather than whichever payload happened to line up badly.
    if let Some(detail) = payload_set_difference(loaded, rebuilt) {
        return Some(RelaunchReason::PayloadSetChanged { detail });
    }

    // The entry moving is the program starting somewhere else.
    if loaded.entry != rebuilt.entry {
        return Some(RelaunchReason::PayloadSetChanged {
            detail: "the entrypoint moved to a different payload".to_owned(),
        });
    }

    let entry = rebuilt.entry_payload()?;
    if !entry.kind.is_hot_swappable() && !entry_is_swappable_hybrid(entry) {
        return Some(RelaunchReason::EntryNotSwappable {
            kind: entry.kind.label(),
        });
    }

    // Every payload the running process cannot re-take must be identical. This
    // is the byte-identity rule, and it is the whole tier decision: the loaded
    // native library is mapped into the process, so it is either the same
    // library or the process is stale.
    for (was, now) in loaded.payloads.iter().zip(&rebuilt.payloads) {
        if was.hash == now.hash {
            continue;
        }
        if now.kind.is_hot_swappable() {
            continue;
        }
        return Some(match now.kind {
            crate::bundle::PayloadKind::NativeLibrary => RelaunchReason::NativeLibraryChanged {
                payload: now.name.clone(),
            },
            crate::bundle::PayloadKind::HybridManifest => RelaunchReason::HybridManifestChanged {
                payload: now.name.clone(),
            },
            kind => RelaunchReason::PayloadChanged {
                payload: now.name.clone(),
                kind: kind.label(),
            },
        });
    }

    None
}

/// Whether a hybrid entrypoint can head a hot-patchable bundle.
///
/// A hybrid manifest is not itself swappable — but it is the entry of every
/// hybrid bundle, and a hybrid bundle whose manifest and library are both
/// unchanged is exactly the `@Runtime`-only edit that tier 1 exists for. So the
/// entry being a hybrid manifest does not block a swap; the manifest *changing*
/// does, and that is checked with the other payloads.
fn entry_is_swappable_hybrid(entry: &PayloadEntry) -> bool {
    entry.kind == crate::bundle::PayloadKind::HybridManifest
}

/// How the two payload lists differ, or `None` if they are the same shape.
fn payload_set_difference(loaded: &BundleManifest, rebuilt: &BundleManifest) -> Option<String> {
    if loaded.payloads.len() != rebuilt.payloads.len() {
        return Some(format!(
            "the bundle had {} payloads and now has {}",
            loaded.payloads.len(),
            rebuilt.payloads.len()
        ));
    }
    for (was, now) in loaded.payloads.iter().zip(&rebuilt.payloads) {
        if was.name != now.name {
            return Some(format!("payload `{}` became `{}`", was.name, now.name));
        }
        if was.kind != now.kind {
            return Some(format!(
                "payload `{}` was a `{}` and is now a `{}`",
                now.name,
                was.kind.label(),
                now.kind.label()
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::PayloadKind;
    use crate::hash::ContentHash;
    use kira_manifest::{BuildProfile, RunnerId};

    fn entry(name: &str, kind: PayloadKind, bytes: &[u8]) -> PayloadEntry {
        PayloadEntry {
            name: name.to_owned(),
            kind,
            hash: ContentHash::of(bytes),
            size: bytes.len() as u64,
        }
    }

    /// A VM-only bundle: one bytecode payload.
    fn vm(bytecode: &[u8]) -> BundleManifest {
        BundleManifest {
            runner: RunnerId::Desktop,
            profile: BuildProfile::Debug,
            payloads: vec![entry("app.kbc", PayloadKind::VmBytecode, bytecode)],
            entry: 0,
        }
    }

    /// A hybrid bundle: manifest, bytecode, native library.
    fn hybrid(manifest: &[u8], bytecode: &[u8], library: &[u8]) -> BundleManifest {
        BundleManifest {
            runner: RunnerId::Desktop,
            profile: BuildProfile::Debug,
            payloads: vec![
                entry("app.khm", PayloadKind::HybridManifest, manifest),
                entry("app.kbc", PayloadKind::VmBytecode, bytecode),
                entry("libapp.dylib", PayloadKind::NativeLibrary, library),
            ],
            entry: 0,
        }
    }

    /// The central case: a bytecode-only edit is a hot patch.
    #[test]
    fn a_bytecode_only_edit_hot_patches() {
        let loaded = vm(b"KBC1 before");
        let rebuilt = vm(b"KBC1 after!");
        assert_eq!(decide(&loaded, &rebuilt, false), ReloadDecision::HotPatch);
    }

    /// The other central case: the native library moving means the loaded
    /// process is stale, whatever the source edit looked like.
    #[test]
    fn a_native_library_change_relaunches() {
        let loaded = hybrid(b"KHM1", b"KBC1", b"\x7fELF old");
        let rebuilt = hybrid(b"KHM1", b"KBC1", b"\x7fELF new");
        assert_eq!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::NativeLibraryChanged {
                    payload: "libapp.dylib".to_owned(),
                }
            }
        );
    }

    /// The case the whole tier exists for: in a hybrid bundle, the bytecode half
    /// changed and the native half did not, so the running process keeps its
    /// loaded library and takes the new code.
    #[test]
    fn a_runtime_only_edit_to_a_hybrid_bundle_hot_patches() {
        let loaded = hybrid(b"KHM1", b"KBC1 before", b"\x7fELF");
        let rebuilt = hybrid(b"KHM1", b"KBC1 after!", b"\x7fELF");
        assert_eq!(decide(&loaded, &rebuilt, false), ReloadDecision::HotPatch);
    }

    /// The manifest is where the VM/native boundary is written down, so a change
    /// to it is a change the loaded native half cannot be told about.
    #[test]
    fn a_hybrid_manifest_change_relaunches() {
        let loaded = hybrid(b"KHM1 before", b"KBC1", b"\x7fELF");
        let rebuilt = hybrid(b"KHM1 after!", b"KBC1", b"\x7fELF");
        assert_eq!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::HybridManifestChanged {
                    payload: "app.khm".to_owned(),
                }
            }
        );
    }

    /// A save that changed nothing must not disturb a running app.
    #[test]
    fn an_identical_bundle_is_unchanged() {
        let loaded = hybrid(b"KHM1", b"KBC1", b"\x7fELF");
        let rebuilt = hybrid(b"KHM1", b"KBC1", b"\x7fELF");
        assert_eq!(decide(&loaded, &rebuilt, false), ReloadDecision::Unchanged);
        assert_eq!(decide(&loaded, &rebuilt, false).mode(), None);
    }

    /// Unchanged wins over the kill switch: turning hot patching off must not
    /// turn a no-op save into a relaunch.
    #[test]
    fn an_identical_bundle_is_unchanged_even_with_hotpatch_disabled() {
        let loaded = vm(b"KBC1");
        assert_eq!(
            decide(&loaded, &loaded.clone(), true),
            ReloadDecision::Unchanged
        );
    }

    /// The kill switch makes every real change a relaunch, so a session can be
    /// run with tier 1 out of the picture entirely.
    #[test]
    fn the_kill_switch_forces_relaunch() {
        let loaded = vm(b"KBC1 before");
        let rebuilt = vm(b"KBC1 after!");
        assert_eq!(
            decide(&loaded, &rebuilt, true),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::DisabledByEnv
            }
        );
    }

    #[test]
    fn an_asset_change_relaunches() {
        let mut loaded = vm(b"KBC1");
        loaded
            .payloads
            .push(entry("logo.png", PayloadKind::Asset, b"old"));
        let mut rebuilt = vm(b"KBC1");
        rebuilt
            .payloads
            .push(entry("logo.png", PayloadKind::Asset, b"new"));

        assert_eq!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::PayloadChanged {
                    payload: "logo.png".to_owned(),
                    kind: "asset",
                }
            }
        );
    }

    #[test]
    fn a_new_payload_relaunches() {
        let loaded = vm(b"KBC1");
        let mut rebuilt = vm(b"KBC1");
        rebuilt
            .payloads
            .push(entry("logo.png", PayloadKind::Asset, b"new"));

        assert!(matches!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::PayloadSetChanged { .. }
            }
        ));
    }

    #[test]
    fn a_renamed_payload_relaunches() {
        let loaded = vm(b"KBC1");
        let rebuilt = BundleManifest {
            payloads: vec![entry("other.kbc", PayloadKind::VmBytecode, b"KBC1")],
            ..vm(b"KBC1")
        };
        assert!(matches!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::PayloadSetChanged { .. }
            }
        ));
    }

    /// A payload keeping its name but changing kind is not the same payload.
    #[test]
    fn a_payload_changing_kind_relaunches() {
        let loaded = vm(b"KBC1");
        let rebuilt = BundleManifest {
            payloads: vec![entry("app.kbc", PayloadKind::Asset, b"KBC1")],
            ..vm(b"KBC1")
        };
        assert!(matches!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::PayloadSetChanged { .. }
            }
        ));
    }

    #[test]
    fn a_bundle_for_another_runner_relaunches() {
        let loaded = vm(b"KBC1 before");
        let rebuilt = BundleManifest {
            runner: RunnerId::Android,
            ..vm(b"KBC1 after!")
        };
        assert_eq!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::BundleIdentityChanged { field: "runner" }
            }
        );
    }

    #[test]
    fn a_bundle_at_another_profile_relaunches() {
        let loaded = vm(b"KBC1 before");
        let rebuilt = BundleManifest {
            profile: BuildProfile::Release,
            ..vm(b"KBC1 after!")
        };
        assert_eq!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::BundleIdentityChanged { field: "profile" }
            }
        );
    }

    /// An entrypoint that is not code cannot be swapped, whatever else matches.
    #[test]
    fn an_unswappable_entrypoint_relaunches() {
        let loaded = BundleManifest {
            payloads: vec![entry("logo.png", PayloadKind::Asset, b"old")],
            entry: 0,
            ..vm(b"KBC1")
        };
        let rebuilt = BundleManifest {
            payloads: vec![entry("logo.png", PayloadKind::Asset, b"new")],
            entry: 0,
            ..vm(b"KBC1")
        };
        assert_eq!(
            decide(&loaded, &rebuilt, false),
            ReloadDecision::Relaunch {
                reason: RelaunchReason::EntryNotSwappable { kind: "asset" }
            }
        );
    }

    /// Every relaunch reason renders a message that names what changed, because
    /// the reason is what the user is told and "restart required" alone is not
    /// an explanation.
    #[test]
    fn every_relaunch_reason_names_what_changed() {
        let reasons = [
            RelaunchReason::NativeLibraryChanged {
                payload: "libapp.dylib".to_owned(),
            },
            RelaunchReason::HybridManifestChanged {
                payload: "app.khm".to_owned(),
            },
            RelaunchReason::PayloadChanged {
                payload: "logo.png".to_owned(),
                kind: "asset",
            },
            RelaunchReason::PayloadSetChanged {
                detail: "a payload appeared".to_owned(),
            },
            RelaunchReason::BundleIdentityChanged { field: "runner" },
            RelaunchReason::EntryNotSwappable { kind: "asset" },
            RelaunchReason::DisabledByEnv,
            RelaunchReason::RunnerRefused {
                reason: "busy".to_owned(),
            },
        ];
        for reason in reasons {
            let rendered = reason.to_string();
            assert!(
                rendered.len() > 20 && !rendered.ends_with(':'),
                "`{reason:?}` renders as `{rendered}`, which explains nothing"
            );
        }
    }

    #[test]
    fn decisions_carry_their_mode() {
        assert_eq!(ReloadDecision::HotPatch.mode(), Some(ReloadMode::HotPatch));
        assert_eq!(
            ReloadDecision::Relaunch {
                reason: RelaunchReason::DisabledByEnv
            }
            .mode(),
            Some(ReloadMode::Relaunch)
        );
        assert_eq!(ReloadDecision::Unchanged.mode(), None);
    }
}
