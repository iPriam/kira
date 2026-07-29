//! Widening one generic instantiation into another whose type arguments are
//! `Any`.
//!
//! [`Type::assignable_to`] answers what one *type* admits without looking
//! anything up, which is why it is a method on [`Type`] and why it cannot answer
//! this question: `Result<Int, E>` and `Result<Any, E>` are two ordinary rows of
//! the enum table, and nothing in either row's [`Type`] says they came from one
//! template. The table knows, so the rule lives here.
//!
//! # The rule
//!
//! [`TypeTable::admits`] is [`Type::assignable_to`] plus exactly one clause: an
//! instantiation widens into another instantiation **of the same template** when
//! every type argument either stays as it was or becomes `Any`. Nothing about
//! this names `Result`. A user's `enum Box<T>` widens by the identical path,
//! because the only thing consulted is what the table recorded when the row was
//! minted.
//!
//! # The boundary, and why it is where it is
//!
//! A widening is a *rebuild* on a statically typed backend — see
//! `kira-llvm-backend`'s `widening` module — so the rule may only admit what the
//! rebuild can carry out. Three consequences, each deliberate:
//!
//! - **Every variant's payload must convert.** A template that puts its
//!   parameter behind something that does not widen — `enum Holder<T> {
//!   Items([T]) }` — does not widen, because `[Int]` is not `[Any]`. The type
//!   arguments widen; a position that merely *contains* one does not.
//! - **An array is invariant, and a struct field is not a widening position at
//!   all.** `[Result<Int, E>]` is not `[Result<Any, E>]` for the same reason
//!   `[Int]` is not `[Any]`: array element types match exactly, and this adds no
//!   exception. A struct is never generic (`KPAR047`), so two structs with
//!   different field types are unrelated nominal types and there is nothing to
//!   widen between.
//! - **Widening composes with itself.** A type argument that is itself an
//!   instantiation widens by this same rule, so `Result<Result<Int, E>, E>`
//!   reaches `Result<Result<Any, E>, E>`, and the rebuild recurses to match.
//!
//! # Termination
//!
//! A template's payload may be an instantiation of itself, so the walk can meet
//! a pair it is already deciding. It answers `true` for one already in progress
//! — the coinductive reading, and the only one that agrees with the generated
//! rebuild, which is a recursive function rather than an infinite one.

use super::{EnumId, Type, TypeTable};

/// How deep the pair stack may go before the walk gives up.
///
/// The in-progress stack already stops a type that meets *itself*; this stops
/// one that grows instead, which is the same thing `KSEM175` caps at
/// instantiation time. Reaching it answers "does not widen", never a hang.
const MAX_WIDENING_DEPTH: usize = 32;

impl TypeTable {
    /// Whether a value of `from` may be used where `from` is declared to be
    /// `to`.
    ///
    /// [`Type::assignable_to`] first, so every rule the lattice already has is
    /// unchanged, and the generic widening only for the pairs it turned down.
    pub fn admits(&self, from: Type, to: Type) -> bool {
        from.assignable_to(to) || self.widens_to(from, to)
    }

    /// Whether `from` widens into `to` by the generic-instantiation rule alone.
    ///
    /// Split out from [`TypeTable::admits`] because a caller inserting the
    /// conversion has to know *which* of the two rules applied: an erasure and a
    /// rebuild are different nodes.
    pub fn widens_to(&self, from: Type, to: Type) -> bool {
        let (Type::Enum(from_id), Type::Enum(to_id)) = (from, to) else {
            return false;
        };
        if from_id == to_id {
            return false;
        }
        self.instantiation_widens(from_id, to_id, &mut Vec::new())
    }

    /// The widening rule for two enum rows, under the pairs already being
    /// decided.
    fn instantiation_widens(
        &self,
        from: EnumId,
        to: EnumId,
        in_progress: &mut Vec<(EnumId, EnumId)>,
    ) -> bool {
        if in_progress.contains(&(from, to)) {
            return true;
        }
        if in_progress.len() >= MAX_WIDENING_DEPTH {
            return false;
        }
        let (Some(from_def), Some(to_def)) = (self.enums().get(from), self.enums().get(to)) else {
            return false;
        };
        let (Some(from_args), Some(to_args)) = (
            self.enums().instantiation(from),
            self.enums().instantiation(to),
        ) else {
            return false;
        };
        if from_args.template != to_args.template
            || from_args.arguments.len() != to_args.arguments.len()
        {
            return false;
        }
        // Two rows of one template always have the same variants; checking it
        // anyway is what lets the rebuild index them in lockstep.
        if from_def.variants.len() != to_def.variants.len() {
            return false;
        }

        in_progress.push((from, to));
        let arguments_widen = from_args
            .arguments
            .iter()
            .zip(to_args.arguments.iter())
            .all(|(&argument, &target)| self.position_widens(argument, target, in_progress));
        let payloads_convert = arguments_widen
            && from_def.variants.iter().zip(to_def.variants.iter()).all(
                |(from_variant, to_variant)| {
                    from_variant.name == to_variant.name
                        && match (from_variant.payload, to_variant.payload) {
                            (None, None) => true,
                            (Some(payload), Some(target)) => {
                                self.position_widens(payload, target, in_progress)
                            }
                            _ => false,
                        }
                },
            );
        in_progress.pop();
        payloads_convert
    }

    /// Whether one position — a type argument or a variant's payload — carries
    /// `from` where `to` is written.
    ///
    /// Exactly three ways: it did not change, it crossed into the top type, or
    /// it is itself an instantiation that widens. Notably *not*
    /// [`Type::assignable_to`]: the numeric wildcard that makes `U8` usable
    /// where `Int` is written is a rule about literals, and reading it as
    /// `Result<U8, E>` -> `Result<Int, E>` would widen a type argument into
    /// something that is not `Any`.
    fn position_widens(
        &self,
        from: Type,
        to: Type,
        in_progress: &mut Vec<(EnumId, EnumId)>,
    ) -> bool {
        if from == to {
            return true;
        }
        if to == Type::Any {
            return from.erases_into_any() && from != Type::Void;
        }
        match (from, to) {
            (Type::Enum(from_id), Type::Enum(to_id)) => {
                self.instantiation_widens(from_id, to_id, in_progress)
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{EnumDef, EnumId, Instantiation, Type, TypeTable, VariantDef};

    /// Declares `Name<args>` with the variants a `Result` has, or whatever
    /// payloads are handed in.
    fn instantiate(
        table: &mut TypeTable,
        template: &str,
        name: &str,
        arguments: Vec<Type>,
        variants: Vec<VariantDef>,
    ) -> EnumId {
        let id = table
            .enums_mut()
            .declare(EnumDef {
                name: name.to_owned(),
                variants,
            })
            .expect("a fresh name declares");
        table.enums_mut().record_instantiation(
            id,
            Instantiation {
                template: template.to_owned(),
                arguments,
            },
        );
        id
    }

    fn variant(name: &str, payload: Option<Type>) -> VariantDef {
        VariantDef {
            name: name.to_owned(),
            payload,
        }
    }

    fn result_variants(ok: Type, failure: Type) -> Vec<VariantDef> {
        vec![variant("Ok", Some(ok)), variant("Error", Some(failure))]
    }

    /// The failure type both instantiations share, hand-written rather than
    /// generic — as `TestFailure` is.
    fn failure(table: &mut TypeTable) -> Type {
        let id = table
            .enums_mut()
            .declare(EnumDef {
                name: "Failure".to_owned(),
                variants: vec![variant("Runtime", Some(Type::String))],
            })
            .expect("a fresh name declares");
        Type::Enum(id)
    }

    #[test]
    fn a_type_argument_widens_into_any() {
        let mut table = TypeTable::new();
        let e = failure(&mut table);
        let narrow = instantiate(
            &mut table,
            "Result",
            "Result<Int, Failure>",
            vec![Type::INT, e],
            result_variants(Type::INT, e),
        );
        let wide = instantiate(
            &mut table,
            "Result",
            "Result<Any, Failure>",
            vec![Type::Any, e],
            result_variants(Type::Any, e),
        );
        assert!(table.admits(Type::Enum(narrow), Type::Enum(wide)));
        assert!(table.widens_to(Type::Enum(narrow), Type::Enum(wide)));
        // One-directional, exactly as `Any` itself is.
        assert!(!table.admits(Type::Enum(wide), Type::Enum(narrow)));
    }

    #[test]
    fn nothing_special_cases_the_name_result() {
        let mut table = TypeTable::new();
        let narrow = instantiate(
            &mut table,
            "Box",
            "Box<String>",
            vec![Type::String],
            vec![variant("Held", Some(Type::String))],
        );
        let wide = instantiate(
            &mut table,
            "Box",
            "Box<Any>",
            vec![Type::Any],
            vec![variant("Held", Some(Type::Any))],
        );
        assert!(table.admits(Type::Enum(narrow), Type::Enum(wide)));
    }

    #[test]
    fn two_templates_with_the_same_shape_stay_unrelated() {
        let mut table = TypeTable::new();
        let narrow = instantiate(
            &mut table,
            "Box",
            "Box<Int>",
            vec![Type::INT],
            vec![variant("Held", Some(Type::INT))],
        );
        let wide = instantiate(
            &mut table,
            "Crate",
            "Crate<Any>",
            vec![Type::Any],
            vec![variant("Held", Some(Type::Any))],
        );
        assert!(!table.admits(Type::Enum(narrow), Type::Enum(wide)));
    }

    #[test]
    fn a_hand_written_enum_never_widens() {
        let mut table = TypeTable::new();
        let plain = table
            .enums_mut()
            .declare(EnumDef {
                name: "Held".to_owned(),
                variants: vec![variant("One", Some(Type::INT))],
            })
            .expect("declares");
        let wide = instantiate(
            &mut table,
            "Box",
            "Box<Any>",
            vec![Type::Any],
            vec![variant("One", Some(Type::Any))],
        );
        assert!(!table.admits(Type::Enum(plain), Type::Enum(wide)));
    }

    #[test]
    fn widening_composes_with_itself() {
        let mut table = TypeTable::new();
        let e = failure(&mut table);
        let inner_narrow = instantiate(
            &mut table,
            "Result",
            "Result<Int, Failure>",
            vec![Type::INT, e],
            result_variants(Type::INT, e),
        );
        let inner_wide = instantiate(
            &mut table,
            "Result",
            "Result<Any, Failure>",
            vec![Type::Any, e],
            result_variants(Type::Any, e),
        );
        let outer_narrow = instantiate(
            &mut table,
            "Result",
            "Result<Result<Int, Failure>, Failure>",
            vec![Type::Enum(inner_narrow), e],
            result_variants(Type::Enum(inner_narrow), e),
        );
        let outer_wide = instantiate(
            &mut table,
            "Result",
            "Result<Result<Any, Failure>, Failure>",
            vec![Type::Enum(inner_wide), e],
            result_variants(Type::Enum(inner_wide), e),
        );
        assert!(table.admits(Type::Enum(outer_narrow), Type::Enum(outer_wide)));
    }

    #[test]
    fn a_parameter_inside_an_array_does_not_widen() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::INT);
        let anys = table.array_of(Type::Any);
        let narrow = instantiate(
            &mut table,
            "Holder",
            "Holder<Int>",
            vec![Type::INT],
            vec![variant("Items", Some(ints))],
        );
        let wide = instantiate(
            &mut table,
            "Holder",
            "Holder<Any>",
            vec![Type::Any],
            vec![variant("Items", Some(anys))],
        );
        // The type arguments would widen; the payload position that *contains*
        // one does not, and the rule refuses rather than admitting something the
        // rebuild cannot carry out.
        assert!(!table.admits(Type::Enum(narrow), Type::Enum(wide)));
        // The reason, stated directly: an array is invariant.
        assert!(!table.admits(ints, anys));
    }

    #[test]
    fn a_type_argument_never_widens_to_a_different_width() {
        let mut table = TypeTable::new();
        let narrow = instantiate(
            &mut table,
            "Box",
            "Box<U8>",
            vec![Type::Int(super::super::IntSpelling::U8)],
            vec![variant(
                "Held",
                Some(Type::Int(super::super::IntSpelling::U8)),
            )],
        );
        let plain = instantiate(
            &mut table,
            "Box",
            "Box<Int>",
            vec![Type::INT],
            vec![variant("Held", Some(Type::INT))],
        );
        // `U8` reaches an `Int` *position* because a literal has to; a type
        // argument is not that position, and this is the only rule that says so.
        assert!(!table.admits(Type::Enum(narrow), Type::Enum(plain)));
    }

    #[test]
    fn a_self_referential_template_terminates() {
        let mut table = TypeTable::new();
        // `enum Chain<T> { More(Chain<T>) Last(T) }`: the payload of `More` is
        // the row being decided, so the walk meets itself.
        let narrow = table
            .enums_mut()
            .declare(EnumDef {
                name: "Chain<Int>".to_owned(),
                variants: Vec::new(),
            })
            .expect("declares");
        let wide = table
            .enums_mut()
            .declare(EnumDef {
                name: "Chain<Any>".to_owned(),
                variants: Vec::new(),
            })
            .expect("declares");
        table.enums_mut().set_variants(
            narrow,
            vec![
                variant("More", Some(Type::Enum(narrow))),
                variant("Last", Some(Type::INT)),
            ],
        );
        table.enums_mut().set_variants(
            wide,
            vec![
                variant("More", Some(Type::Enum(wide))),
                variant("Last", Some(Type::Any)),
            ],
        );
        table.enums_mut().record_instantiation(
            narrow,
            Instantiation {
                template: "Chain".to_owned(),
                arguments: vec![Type::INT],
            },
        );
        table.enums_mut().record_instantiation(
            wide,
            Instantiation {
                template: "Chain".to_owned(),
                arguments: vec![Type::Any],
            },
        );
        assert!(table.admits(Type::Enum(narrow), Type::Enum(wide)));
    }

    #[test]
    fn a_void_argument_never_widens() {
        let mut table = TypeTable::new();
        let narrow = instantiate(
            &mut table,
            "Box",
            "Box<Void>",
            vec![Type::Void],
            vec![variant("Held", Some(Type::Void))],
        );
        let wide = instantiate(
            &mut table,
            "Box",
            "Box<Any>",
            vec![Type::Any],
            vec![variant("Held", Some(Type::Any))],
        );
        // `Void` names no value, so there is nothing to erase — the same arm
        // `Type::assignable_to` already has for it.
        assert!(!table.admits(Type::Enum(narrow), Type::Enum(wide)));
    }

    #[test]
    fn a_row_is_not_a_widening_of_itself() {
        let mut table = TypeTable::new();
        let id = instantiate(
            &mut table,
            "Box",
            "Box<Int>",
            vec![Type::INT],
            vec![variant("Held", Some(Type::INT))],
        );
        // Assignable, yes; a *widening*, no — nothing has to be rebuilt.
        assert!(table.admits(Type::Enum(id), Type::Enum(id)));
        assert!(!table.widens_to(Type::Enum(id), Type::Enum(id)));
    }
}
