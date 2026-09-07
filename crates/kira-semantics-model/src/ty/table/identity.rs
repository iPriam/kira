//! Package-qualified identity, answered from the table's rows.

use super::TypeTable;
use crate::ty::Type;
use crate::ty::identity::{NominalIdentity, NominalKind, PackageIdentity};
use crate::ty::structs::StructOrigin;

impl TypeTable {
    /// Records a package the program was assembled from. A name already
    /// recorded keeps its first identity.
    pub fn declare_package(&mut self, package: PackageIdentity) {
        if !self.packages.iter().any(|known| known.name == package.name) {
            self.packages.push(package);
        }
    }

    /// The identity of the package named `name`; a package the assembly never
    /// described is identified by its name alone.
    pub fn package(&self, name: &str) -> PackageIdentity {
        self.packages
            .iter()
            .find(|package| package.name == name)
            .cloned()
            .unwrap_or_else(|| PackageIdentity {
                name: name.to_owned(),
                version: String::new(),
                instance: String::new(),
            })
    }

    /// The nominal identity of `ty`, or `None` for a type that is not a
    /// declaration (a scalar, an array, `Any`, …).
    pub fn identity(&self, ty: Type) -> Option<NominalIdentity> {
        let (owner, module, name, kind, arguments) = match ty {
            Type::Struct(id) => {
                let def = self.structs.get(id)?;
                let kind = match self.structs.origin(id) {
                    StructOrigin::FunctionType => NominalKind::FunctionType,
                    StructOrigin::Declared => NominalKind::Struct,
                };
                (
                    self.structs.owner_of(id),
                    self.structs.module_of(id),
                    def.name.as_str(),
                    kind,
                    self.structs.instantiation(id),
                )
            }
            Type::Enum(id) => {
                let def = self.enums.get(id)?;
                let kind = if def.name.starts_with("Any ") {
                    NominalKind::ConstructFamily
                } else if def.name.starts_with("some ") {
                    NominalKind::TraitExistential
                } else {
                    NominalKind::Enum
                };
                (
                    self.enums.owner_of(id),
                    self.enums.module_of(id),
                    def.name.as_str(),
                    kind,
                    self.enums.instantiation(id),
                )
            }
            Type::Distinct(id) => {
                let def = self.distincts.get(id)?;
                (
                    self.distincts.owner_of(id),
                    self.distincts.module_of(id),
                    def.name.as_str(),
                    NominalKind::Distinct,
                    None,
                )
            }
            _ => return None,
        };
        // An instantiation's row name carries the spelled arguments; the
        // identity keeps the template's name and the arguments apart.
        let (name, arguments) = match arguments {
            Some(instantiation) => (
                name.split('<').next().unwrap_or(name).to_owned(),
                instantiation.arguments.clone(),
            ),
            None => (name.to_owned(), Vec::new()),
        };
        Some(NominalIdentity {
            package: owner.map(|owner| self.package(owner)),
            module: module.to_owned(),
            name,
            kind,
            arguments,
        })
    }

    /// The identity of `ty` as one string, recursively through generic
    /// arguments: what a fingerprint, a mangled symbol, or a serialized name
    /// is built from. A type with no nominal identity spells as its name.
    pub fn identity_key(&self, ty: Type) -> String {
        match self.identity(ty) {
            Some(identity) => {
                let mut key = identity.declaration_key();
                if !identity.arguments.is_empty() {
                    key.push('<');
                    for (index, &argument) in identity.arguments.iter().enumerate() {
                        if index > 0 {
                            key.push(',');
                        }
                        key.push_str(&self.identity_key(argument));
                    }
                    key.push('>');
                }
                key
            }
            None => match self.element_of(ty) {
                Some(element) if matches!(ty, Type::Array(_)) => {
                    format!("[{}]", self.identity_key(element))
                }
                _ => self.type_name(ty),
            },
        }
    }
}
