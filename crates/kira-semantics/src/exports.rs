//! The `@Export` surface: which functions a library offers a consumer, what a
//! name maps to, and everything the boundary refuses.
//!
//! `@Export` is **new Kira design**, not oracle behavior — the oracle has no
//! library-export or embedding concept, and its `@FFI.*` family runs the other
//! direction entirely. What is pinned, and obeyed here, is the annotation
//! grammar the marker rides on.
//!
//! # Why the checks live in the frontend
//!
//! Every rule below has the same answer on every backend: whether a type can
//! cross the boundary is a property of the type, not of the engine that carries
//! it. Putting them here — above the backend split, beside the entrypoint rule
//! [`crate::BuildKind`] already decides — is what keeps three engines from
//! each growing their own opinion about what an export is.
//!
//! # What the boundary refuses, and why each one is a refusal
//!
//! The refusals are the standing never-travels set plus the two the export
//! boundary adds. An array is refused because who frees its elements is
//! undesigned; a struct and an enum because neither fits one tag and one word;
//! a function value because it has no crossing representation at all. A class
//! that is not itself `@Export` is refused rather than silently exported,
//! because handle-eligibility is the author's decision. `move` and
//! `borrow mut` are refused because the boundary contract is fixed per type:
//! a mutable borrow across it would promise mutation of storage the other side
//! does not manage.
//!
//! Refusing beats guessing here for the same reason `print(struct)` and
//! struct-at-the-native-seam are refused: an invented answer becomes the
//! contract the moment anything depends on it.

use std::collections::HashSet;

use kira_semantics_model::hir::{FuncId, HirExport};
use kira_semantics_model::{OwnershipMode, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ExportMark, Item};

use crate::analyze::{Analyzer, Callable};
use crate::build_kind::BuildKind;

/// The name a consumer calls an exported Kira function by.
///
/// The mapping is lowerCamelCase to snake_case: `makeButton` becomes
/// `make_button`, `clickAt` becomes `click_at`, and an acronym run breaks
/// before its last capital, so `parseHTTPHeader` becomes `parse_http_header`.
/// It is derived and never written — `@Export` takes no symbol override, so
/// this function is the whole of the naming contract.
///
/// The mapping is deliberately not injective (`buttonLabel` and `button_label`
/// both land on `button_label`), which is exactly why a collision is checked
/// rather than assumed away.
pub fn exported_name(kira_name: &str) -> String {
    let chars: Vec<char> = kira_name.chars().collect();
    let mut out = String::with_capacity(chars.len() + 4);
    for (index, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_uppercase() {
            let previous_is_lower_or_digit = index > 0
                && (chars[index - 1].is_ascii_lowercase() || chars[index - 1].is_ascii_digit());
            // The last capital of an acronym run belongs to the next word:
            // `HTTPHeader` breaks before `H`, not before `TTPH`.
            let starts_new_word = index > 0
                && chars[index - 1].is_ascii_uppercase()
                && chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if (previous_is_lower_or_digit || starts_new_word) && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

impl Analyzer<'_> {
    /// Checks every `@Export` in the program and records the surface that
    /// survives.
    ///
    /// Runs after signatures are collected, because an export's refusals are
    /// about its *resolved* parameter and return types, and after classes are
    /// flattened, because handle-eligibility is a property of a class in the
    /// struct table.
    pub(crate) fn check_exports(&mut self, callables: &[Callable<'_>]) {
        let exported_classes = self.exported_classes();
        // A collision is reported against the second spelling: the first one is
        // the one that stays valid, so the author renames the one they just hit.
        let mut taken: HashSet<String> = HashSet::new();
        for (index, callable) in callables.iter().enumerate() {
            let Some(mark) = callable.function.export else {
                continue;
            };
            // A specialization copy shares the original's `@Export` mark and
            // name — it is the same function registered again with a parameter
            // narrowed to a subclass. Checking it here would report one legal
            // export twice: once as a second mapping to the same consumer
            // name, and once more per copy for a method mark. The original,
            // which carries the export for real, is what this loop checks.
            if !callable.specialize.is_empty() {
                continue;
            }
            self.source = callable.source;
            let name_span = callable.function.name_span;
            if callable.receiver.is_some() {
                // A class mints handles; it does not publish its methods. The
                // author wraps a method in an exported free function, which is
                // sugar this can grow later without an ABI change.
                self.emit(
                    mark.span,
                    "KSEM167",
                    "`@Export` cannot annotate a method: a library exports \
                     top-level functions, and an `@Export` class only makes its \
                     instances crossable as handles. Wrap the method in an \
                     exported function.",
                );
                continue;
            }
            if !self.check_export_is_allowed_here(mark, name_span) {
                continue;
            }
            let kira_name = self.interner.resolve(callable.function.name).to_owned();
            let id = FuncId(index as u32);
            let signature_is_clean = self.check_export_signature(id, callable, &exported_classes);
            let mapped = exported_name(&kira_name);
            if taken.contains(&mapped) {
                self.emit(
                    name_span,
                    "KSEM168",
                    format!(
                        "two exports map to the consumer name `{mapped}`: exported \
                         names are derived by snake_casing, so `{kira_name}` collides \
                         with an export declared earlier. Rename one."
                    ),
                );
                continue;
            }
            taken.insert(mapped.clone());
            if signature_is_clean {
                // Read off the collected signature, never re-resolved: the
                // surface a consumer is generated against has to be the same
                // types the checks above just approved.
                self.program.exports.push(HirExport {
                    kira_name,
                    exported_name: mapped,
                    function: id,
                    params: self.param_types(id),
                    result: self.signature_return_type(id),
                });
            }
        }
    }

    /// Whether this `@Export` may appear at all: the package must be a library
    /// and the marker must be bare.
    fn check_export_is_allowed_here(&mut self, mark: ExportMark, name_span: Span) -> bool {
        let mut allowed = true;
        if self.build_kind != BuildKind::Library {
            self.emit(
                name_span,
                "KSEM256",
                "`@Export` is only meaningful in a library package: an \
                 application is entered at its `@Main`, not called by a \
                 consumer. Set `let kind = .Library` in `package.kira`.",
            );
            allowed = false;
        }
        if let Some(payload) = mark.payload_span {
            self.emit(
                payload,
                "KSEM166",
                "`@Export` takes no arguments and no block: the consumer-facing \
                 name is derived from the function's own name, and a symbol \
                 override is surface nothing needs yet.",
            );
            allowed = false;
        }
        allowed
    }

    /// Checks an exported function's parameters and result against the
    /// boundary contract, returning whether all of them may cross.
    fn check_export_signature(
        &mut self,
        id: FuncId,
        callable: &Callable<'_>,
        exported_classes: &[StructId],
    ) -> bool {
        let modes = self.param_ownership(id);
        // Types come from the collected signature, never from re-resolving what
        // was written: resolution reports an unknown name every time it runs,
        // so re-resolving here would report the same unknown type a second
        // time. A method is refused before this point, so slot 0 is a real
        // parameter and the two sequences align.
        let types = self.param_types(id);
        let mut clean = true;
        for (index, param) in callable.function.params.iter().enumerate() {
            let mode = modes.get(index).copied().unwrap_or(OwnershipMode::Owned);
            if matches!(mode, OwnershipMode::Move | OwnershipMode::BorrowMut) {
                let span = param.ownership_span.unwrap_or(param.span);
                self.emit(
                    span,
                    "KSEM165",
                    format!(
                        "an exported parameter may not declare `{}`: the export \
                         boundary's ownership is fixed per type — scalars and \
                         handles copy, a string is lent — so a per-parameter mode \
                         would promise something the consumer's side cannot honor.",
                        mode.spelling()
                    ),
                );
                clean = false;
            }
            // A parameter with no collected type is one the signature pass
            // already refused; `Error` is what it recorded, and `Error` crosses
            // silently.
            let ty = types.get(index).copied().unwrap_or(Type::Error);
            clean &= self.check_export_type(ty, param.span, "parameter", exported_classes);
        }
        // A written result is checked against the span the author wrote it at;
        // an absent one is `Void`, which always crosses and has no span.
        if let Some(written) = callable.function.return_type {
            let span = self.tree.type_ref(written).span();
            let return_type = self.signature_return_type(id);
            clean &= self.check_export_type(return_type, span, "result", exported_classes);
        }
        clean
    }

    /// Whether `ty` may cross the export boundary, reporting the reason when it
    /// may not.
    ///
    /// `Error` passes silently: whatever produced it already spoke, and a
    /// second diagnostic about a type nobody successfully wrote is noise.
    fn check_export_type(
        &mut self,
        ty: Type,
        span: Span,
        position: &str,
        exported_classes: &[StructId],
    ) -> bool {
        let name = self.type_name(ty);
        // Asked before the shape questions below, because it is not one: a
        // handle a consumer holds outlives every Kira frame, and the release
        // that frees it runs outside any engine — there is nothing to enter the
        // body with. See `Instance::release`.
        if self.program.types.runs_user_drop(ty) {
            self.emit(
                span,
                "KSEM303",
                format!(
                    "`{name}` runs a user `Drop` body, so it cannot be an export {position}: a \
                     consumer holds it past every Kira frame, and the release that frees it \
                     happens where no engine is running to enter the body. Every user `Drop` \
                     body runs before the run that made the value ends."
                ),
            );
            return false;
        }
        match ty {
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            | Type::Void
            | Type::Error => true,
            // A distinct type crosses as the representation it is. The wrapper
            // a consumer generates names that scalar, because that is the whole
            // of what leaves the boundary — the nominal half of a distinct type
            // is a Kira-side fact, and no exported C prototype has a place to
            // keep it.
            Type::Distinct(_) => self.check_export_type(
                self.program.types.representation(ty),
                span,
                position,
                exported_classes,
            ),
            // The C-seam types belong to the `@FFI.Extern` import direction, not
            // the `@Export` boundary: `RawPtr` is an opaque host word Kira never
            // interprets, and `CString` is borrowed C storage with no owned
            // representation. Neither is part of the surface a consumer's wrapper
            // is generated against, so both are refused here. (A written
            // `CString` in an ordinary position is already `Error` by `KSEM176`,
            // so only `RawPtr` reaches this arm in practice.)
            // A capture cell is refused here too, and for the strongest reason
            // on this list: it is shared mutable storage whose share count this
            // runtime owns. Nothing outside can hold one without a count nobody
            // manages. It is not surface either, so no author can reach this
            // arm by writing a signature — it exists so the desugar cannot leak
            // one through an export.
            Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::CString
            | Type::CBlock
            | Type::RuntimeType
            | Type::NativeState(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::Cell(_) => {
                self.emit(
                    span,
                    "KSEM186",
                    format!(
                        "`{name}` cannot cross the export boundary: `RawPtr` and \
                         `CString` are `@FFI.Extern` seam types for calling *into* C, \
                         not part of a library's exported surface."
                    ),
                );
                false
            }
            // The export boundary carries one tag and one word, and `Any` is one
            // word — but a consumer's generated wrapper has to *name* the type
            // it gets back, and `Any` names none. So this is refused for the
            // same reason the C seam refuses it, not for the aggregate reason
            // the arms below give.
            Type::Any => {
                self.emit(
                    span,
                    "KSEM186",
                    format!(
                        "`{name}` cannot cross the export boundary: an erased value \
                         has no type a consumer's wrapper could name. Export the \
                         concrete type."
                    ),
                );
                false
            }
            Type::Array(_) => {
                self.emit(
                    span,
                    "KSEM160",
                    format!(
                        "an array cannot cross the export boundary yet: `{name}` as \
                         an export {position} leaves who frees the elements \
                         undesigned. Pass the elements one at a time, or return a \
                         handle to an `@Export` class that owns them."
                    ),
                );
                false
            }
            Type::Enum(_) => {
                self.emit(
                    span,
                    "KSEM162",
                    format!(
                        "an enum cannot cross the export boundary: `{name}` as an \
                         export {position} is a tagged value, and the boundary \
                         carries one tag and one word. Pass the payload, or wrap it \
                         in an `@Export` class."
                    ),
                );
                false
            }
            Type::Struct(id) => {
                if self.as_function_type(ty).is_some() {
                    self.emit(
                        span,
                        "KSEM163",
                        format!(
                            "a function value cannot cross the export boundary: \
                             `{name}` as an export {position} has no crossing \
                             representation. A consumer calling back into Kira is \
                             just another export; Kira calling out is the native \
                             import direction."
                        ),
                    );
                    return false;
                }
                if !self.classes.contains_key(&id) {
                    self.emit(
                        span,
                        "KSEM161",
                        format!(
                            "a struct cannot cross the export boundary by value: \
                             `{name}` as an export {position} does not fit one tag \
                             and one word. Declare it a class, mark it `@Export`, \
                             and pass a handle instead."
                        ),
                    );
                    return false;
                }
                if !exported_classes.contains(&id) {
                    self.emit(
                        span,
                        "KSEM164",
                        format!(
                            "`{name}` is not an exported class, so it cannot be an \
                             export {position}: only an `@Export` class crosses as \
                             a handle. Mark `{name}` `@Export`."
                        ),
                    );
                    return false;
                }
                true
            }
        }
    }

    /// Every class the program marked `@Export`, as struct ids.
    ///
    /// A class is a struct by the time anything downstream sees it, so
    /// handle-eligibility is recorded as the set of struct ids classes were
    /// declared as — which is also the form the type checks above compare
    /// against.
    fn exported_classes(&mut self) -> Vec<StructId> {
        // Collected first, then reported: `emit` needs `&mut self` and the walk
        // borrows the tree out of it.
        let mut found: Vec<(SourceId, ExportMark, Option<StructId>)> = Vec::new();
        for (source, item) in self.tree.items_with_source() {
            let Item::Class(declaration) = item else {
                continue;
            };
            let Some(mark) = declaration.export else {
                continue;
            };
            // A class that failed to flatten has no row in the struct table and
            // was already reported; its marker adds no second diagnostic.
            let id = self
                .program
                .types
                .structs()
                .lookup(self.interner.resolve(declaration.name));
            found.push((source, mark, id));
        }
        let mut exported = Vec::new();
        for (source, mark, id) in found {
            self.source = source;
            let name_span = mark.span;
            if self.check_export_is_allowed_here(mark, name_span) {
                // Only a class the package may export at all becomes
                // handle-eligible; refusing the marker and then honoring it
                // would make the refusal a no-op.
                exported.extend(id);
            }
        }
        exported
    }
}

#[cfg(test)]
mod tests {
    use super::exported_name;

    #[test]
    fn a_camel_case_name_becomes_snake_case() {
        assert_eq!(exported_name("makeButton"), "make_button");
        assert_eq!(exported_name("clickAt"), "click_at");
        assert_eq!(exported_name("buttonLabel"), "button_label");
    }

    #[test]
    fn an_already_snake_case_name_is_unchanged() {
        assert_eq!(exported_name("button_label"), "button_label");
        assert_eq!(exported_name("add"), "add");
    }

    #[test]
    fn an_acronym_run_breaks_before_its_last_capital() {
        assert_eq!(exported_name("parseHTTPHeader"), "parse_http_header");
        assert_eq!(exported_name("toJSON"), "to_json");
    }

    #[test]
    fn a_digit_starts_a_new_word_before_a_capital() {
        assert_eq!(exported_name("utf8Decode"), "utf8_decode");
        assert_eq!(exported_name("v2Button"), "v2_button");
    }

    #[test]
    fn the_mapping_is_not_injective_which_is_why_collisions_are_checked() {
        // Both spellings land on the same consumer name. That is the whole
        // reason `KSEM168` exists rather than the mapping being assumed safe.
        assert_eq!(exported_name("buttonLabel"), exported_name("button_label"));
    }
}
