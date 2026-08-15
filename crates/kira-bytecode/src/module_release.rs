//! The release section: which local slots each function's frame releases.
//!
//! Split from `module.rs` alongside the foreign section, and for the same
//! reason: a section is a codec plus the truncation and rejection tests that
//! prove it, and those belong together.
//!
//! The section is appended last and omitted when every function asks for
//! [`FrameRelease::EveryLocal`], so a module built by hand is byte-for-byte
//! what it was before the section existed. That absence is the whole
//! compatibility story — a decoder that finds no section reads the frame
//! discipline the VM has always had.

use crate::module::{
    Format, FrameRelease, FuncProto, ModuleDecodeError, Reader, write_count, write_u64,
};

/// The slot-count value that means "every local", rather than a list.
///
/// A sentinel in the existing `u32` rather than a discriminant byte, on the
/// same terms as the module's `NO_ENTRYPOINT`: no real function has 4294967295
/// slots to release.
const EVERY_LOCAL_LEGACY: u32 = u32::MAX;
const EVERY_LOCAL_WIDE: u64 = u64::MAX;

/// Writes the current release section: one entry per function, in function
/// order. KBC1 is read-only compatibility; no legacy writer narrows a wide
/// plan back to `u16`.
pub(crate) fn write_releases(out: &mut Vec<u8>, functions: &[FuncProto]) {
    write_count(out, functions.len());
    for function in functions {
        match &function.releases {
            FrameRelease::EveryLocal => write_u64(out, EVERY_LOCAL_WIDE),
            FrameRelease::Planned(slots) => {
                write_count(out, slots.len());
                for &slot in slots {
                    out.extend_from_slice(&slot.to_le_bytes());
                }
            }
        }
    }
}

/// Reads the release section into `functions`, leaving them as decoded when
/// there is none.
///
/// A section that names a different number of functions than the module has is
/// refused rather than applied as far as it goes: the entries are positional,
/// so a count that disagrees means the writer and this reader do not agree on
/// what a position names, and releasing by a misaligned plan would free one
/// function's slots on another's frame.
pub(crate) fn read_releases(
    reader: &mut Reader<'_>,
    functions: &mut [FuncProto],
    format: Format,
) -> Result<(), ModuleDecodeError> {
    if reader.is_at_end() {
        return Ok(());
    }
    let entries = reader.read_count(format)?;
    if entries != functions.len() as u64 {
        return Err(ModuleDecodeError::ReleaseCountMismatch {
            functions: functions.len() as u64,
            entries,
        });
    }
    for function in functions.iter_mut() {
        let count = reader.read_count(format)?;
        let every_local = match format {
            Format::Legacy => u64::from(EVERY_LOCAL_LEGACY),
            Format::Wide => EVERY_LOCAL_WIDE,
        };
        function.releases = match count {
            value if value == every_local => FrameRelease::EveryLocal,
            count => {
                let mut slots = Vec::new();
                for _ in 0..count {
                    slots.push(match format {
                        Format::Legacy => u64::from(reader.read_u16()?),
                        Format::Wide => reader.read_u64()?,
                    });
                }
                FrameRelease::Planned(slots)
            }
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{MAGIC, Module};
    use crate::op::Instruction;
    use kira_runtime_abi::Execution;

    fn func(name: &str, releases: FrameRelease) -> FuncProto {
        FuncProto {
            name: name.to_owned(),
            param_count: 0,
            local_count: 3,
            execution: Execution::Runtime,
            code: vec![Instruction::ReturnVoid],
            releases,
        }
    }

    fn module(releases: FrameRelease) -> Module {
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            main: Some(0),
            strings: Vec::new(),
            functions: vec![func("main", releases)],
        }
    }

    #[test]
    fn a_planned_release_round_trips_through_bytes() {
        let module = module(FrameRelease::Planned(vec![0, 2]));
        let decoded = Module::from_bytes(&module.to_bytes()).unwrap();
        assert_eq!(decoded, module);
        assert_eq!(
            decoded.functions[0].releases,
            FrameRelease::Planned(vec![0, 2])
        );
    }

    /// An empty plan is a plan, and must not decode as "release everything" —
    /// the two ask for opposite work on a frame holding strings.
    #[test]
    fn an_empty_plan_is_not_the_absent_section() {
        let empty = module(FrameRelease::Planned(Vec::new()));
        let absent = module(FrameRelease::EveryLocal);
        assert_ne!(empty.to_bytes(), absent.to_bytes());
        assert_eq!(
            Module::from_bytes(&empty.to_bytes()).unwrap().functions[0].releases,
            FrameRelease::Planned(Vec::new())
        );
    }

    /// The compatibility claim, tested rather than asserted.
    #[test]
    fn every_local_writes_no_section_at_all() {
        let bytes = module(FrameRelease::EveryLocal).to_bytes();
        assert_eq!(&bytes[0..4], &MAGIC);
        assert_eq!(
            Module::from_bytes(&bytes).unwrap().functions[0].releases,
            FrameRelease::EveryLocal
        );
        // No section: the bytes end where the function table does, exactly as
        // a build without this section would have written them.
        assert!(bytes.len() < module(FrameRelease::Planned(vec![0])).to_bytes().len());
    }

    #[test]
    fn the_every_local_sentinel_is_pinned_in_the_bytes() {
        // A mixed module: one function planned, so the section is written, and
        // one asking for every local, so the sentinel appears inside it.
        let mut mixed = module(FrameRelease::Planned(vec![1]));
        mixed
            .functions
            .push(func("other", FrameRelease::EveryLocal));
        let bytes = mixed.to_bytes();
        assert_eq!(&bytes[bytes.len() - 8..], &[0xff; 8]);
        assert_eq!(EVERY_LOCAL_WIDE, u64::MAX);
        assert_eq!(Module::from_bytes(&bytes).unwrap(), mixed);
    }

    /// No prefix ever yields a *partial* plan.
    ///
    /// A cut is rejected, or it lands exactly where some earlier section ends
    /// and reads as the module a build without this section would have written
    /// — releasing every local, which is the safe reading. What must never
    /// happen is the third outcome: a plan shorter than the one written, which
    /// would leak whatever the missing entries named.
    #[test]
    fn no_truncation_of_the_release_section_yields_a_partial_plan() {
        let bytes = module(FrameRelease::Planned(vec![0, 2])).to_bytes();
        let complete = bytes.len();
        for cut in 0..complete {
            match Module::from_bytes(&bytes[..cut]) {
                Err(_) => {}
                Ok(decoded) => assert_eq!(
                    decoded.functions[0].releases,
                    FrameRelease::EveryLocal,
                    "prefix of {cut}/{complete} bytes decoded as a plan"
                ),
            }
        }
        assert_eq!(
            Module::from_bytes(&bytes).unwrap().functions[0].releases,
            FrameRelease::Planned(vec![0, 2])
        );
    }

    #[test]
    fn a_section_naming_the_wrong_number_of_functions_is_refused() {
        let mut bytes = module(FrameRelease::Planned(vec![0])).to_bytes();
        // The entry count opens the section; the module has one function.
        let count = bytes.len() - 8 - 8 - 8;
        bytes[count..count + 8].copy_from_slice(&2u64.to_le_bytes());
        assert_eq!(
            Module::from_bytes(&bytes).unwrap_err(),
            ModuleDecodeError::ReleaseCountMismatch {
                functions: 1,
                entries: 2,
            }
        );
    }
}
