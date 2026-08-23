//! Retained-parameter manifest tail decoding.

use super::*;

/// Reads the appended retained-parameter rows onto `foreign`.
pub(super) fn read_foreign_retained(
    reader: &mut Reader<'_>,
    foreign: &mut [HybridForeign],
) -> Result<(), ManifestDecodeError> {
    if reader.is_at_end() {
        return Ok(());
    }
    let rows = reader.u32()?;
    if rows as usize != foreign.len() {
        return Err(ManifestDecodeError::RetainedRowMismatch {
            rows,
            imports: foreign.len(),
        });
    }
    for import in foreign {
        let count = reader.count()?;
        let params = import.signature.parameters().len();
        let mut retained = vec![false; params];
        for _ in 0..count {
            let position = reader.u32()?;
            let Some(slot) = retained.get_mut(position as usize) else {
                return Err(ManifestDecodeError::RetainedOutOfRange {
                    import: import.symbol.clone(),
                    position,
                    params,
                });
            };
            if *slot {
                return Err(ManifestDecodeError::DuplicateRetainedPosition {
                    import: import.symbol.clone(),
                    position,
                });
            }
            *slot = true;
        }
        import.signature = import.signature.clone().with_retained(retained);
    }
    Ok(())
}
