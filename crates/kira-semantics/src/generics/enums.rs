//! Generic enum declaration and substitution.

use super::*;

impl<'a> Analyzer<'a> {
    /// Registers `declaration` as a generic template, or reports why it cannot
    /// be one.
    ///
    /// Returns `true` when the declaration was generic — the caller then skips
    /// declaring it as an ordinary enum, because a template names no type.
    pub(crate) fn register_generic_enum(
        &mut self,
        declaration: &'a EnumDecl,
        source: SourceId,
    ) -> bool {
        if declaration.type_params.is_empty() {
            return false;
        }
        let name = self.interner.resolve(declaration.name).to_owned();
        let mut seen: Vec<String> = Vec::with_capacity(declaration.type_params.len());
        for param in &declaration.type_params {
            let param_name = self.interner.resolve(param.name).to_owned();
            if Type::from_name(&param_name).is_some() {
                self.emit(
                    param.span,
                    "KSEM170",
                    format!(
                        "type parameter `{param_name}` of enum `{name}` shadows a builtin type; \
                         pick a name no type already has"
                    ),
                );
                continue;
            }
            if seen.contains(&param_name) {
                self.emit(
                    param.span,
                    "KSEM171",
                    format!("enum `{name}` already declares a type parameter `{param_name}`"),
                );
                continue;
            }
            seen.push(param_name.clone());
            for bound in &param.bounds {
                let bound_name = self.interner.resolve(bound.name).to_owned();
                // A compiler-known trait is known here without a declaration,
                // so it names a bound the same way it names a conformance.
                if is_builtin_trait(&bound_name) || self.traits.contains_key(&bound_name) {
                    continue;
                }
                self.emit(
                    bound.span,
                    "KSEM289",
                    format!(
                        "`{bound_name}` is not a trait, so it cannot bound `{name}`'s type \
                         parameter `{param_name}`"
                    ),
                );
            }
        }
        if self.generic_enums.contains_key(&name)
            || self.visible_enum(&name).is_some()
            || self.visible_struct(&name).is_some()
        {
            self.emit(
                declaration.name_span,
                "KSEM169",
                format!("generic enum `{name}` is already defined"),
            );
            return true;
        }
        self.generic_enums.insert(
            name,
            GenericEnum {
                decl: declaration,
                source,
            },
        );
        true
    }

    /// Whether `name` is a registered generic enum, so a bare use of it can say
    /// what is missing rather than "unknown type".
    pub(crate) fn is_generic_enum(&self, name: &str) -> bool {
        self.generic_enums.contains_key(name)
    }

    /// The instantiation of the template `name` that `expected` already asks
    /// for, if that is what it asks for.
    ///
    /// This is what lets `Result.Ok(1)` mean the same thing `.Ok(1)` does: a
    /// qualified spelling carries no type arguments, so the *position* is what
    /// supplies them, and the template name written in front only has to agree
    /// with what the position already said. It agrees when the expected enum is
    /// an instantiation of exactly this template — `Result<Int, Bool>` for
    /// `Result`, and not for `Outcome`.
    pub(crate) fn generic_instantiation_expected(
        &self,
        name: &str,
        expected: Option<Type>,
    ) -> Option<EnumId> {
        let Some(Type::Enum(id)) = expected else {
            return None;
        };
        (self.program.types.enums().template_of(id) == Some(name)).then_some(id)
    }

    /// Resolves the type parameter `name` stands for in the substitution
    /// currently in force, if any.
    pub(crate) fn bound_type_param(&self, name: &str) -> Option<Type> {
        self.type_bindings
            .iter()
            .rev()
            .find(|(bound, _)| bound == name)
            .map(|&(_, ty)| ty)
    }
}
