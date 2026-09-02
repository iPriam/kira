//! Package-qualified nominal identity.
//!
//! A nominal declaration is identified by the package that declared it, the
//! module inside that package, its declared name, and — for an instantiated
//! generic — the identity of every argument. Source spelling plays no part
//! once a name is resolved: two packages may each declare a `Point`, and the
//! two are different types wherever identity is asked — type keys, generic
//! instantiation, mangling, native-state fingerprints, serialized names, and
//! the runtime type descriptor.

use super::Type;

/// The resolved identity of one package in the program.
///
/// The name alone is what `import` resolves against; the version and the
/// dependency instance (the canonical root the package was resolved from)
/// are what make two builds of "the same" package different identities when
/// they are.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PackageIdentity {
    /// The package name its manifest declares.
    pub name: String,
    /// The package version its manifest declares.
    pub version: String,
    /// The dependency instance: the canonical directory the package was
    /// resolved from, or empty when the package was not resolved from disk.
    pub instance: String,
}

impl PackageIdentity {
    /// The identity as one string, `name@version#instance`.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}@{}#{}", self.name, self.version, self.instance)
    }
}

/// The identity of one nominal type: where it was declared, what it was
/// declared as, and what it was instantiated with.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NominalIdentity {
    /// The declaring package, or `None` for the program's own files.
    pub package: Option<PackageIdentity>,
    /// The declaring module's identity inside its package.
    pub module: String,
    /// The declared name, as written.
    pub name: String,
    /// The kind of declaration.
    pub kind: NominalKind,
    /// The generic arguments an instantiation was minted with, each already
    /// resolved; empty for a declaration that is not an instantiation.
    pub arguments: Vec<Type>,
}

/// What kind of nominal declaration an identity names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NominalKind {
    Struct,
    Class,
    Enum,
    Distinct,
    ConstructFamily,
    TraitExistential,
    FunctionType,
}

impl NominalKind {
    /// The kind as a word, for descriptors and diagnostics.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            NominalKind::Struct => "struct",
            NominalKind::Class => "class",
            NominalKind::Enum => "enum",
            NominalKind::Distinct => "distinct",
            NominalKind::ConstructFamily => "construct family",
            NominalKind::TraitExistential => "trait existential",
            NominalKind::FunctionType => "function type",
        }
    }
}

impl NominalIdentity {
    /// The package-qualified name a person reads: `Pkg::Name`, or the bare
    /// name for the program's own declaration.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        match &self.package {
            Some(package) => format!("{}::{}", package.name, self.name),
            None => self.name.clone(),
        }
    }

    /// The declaration's identity as one string: package key, module, and
    /// name. Arguments are appended by whoever spells them, since a `Type`
    /// argument needs the table to be spelled.
    #[must_use]
    pub fn declaration_key(&self) -> String {
        let package = self
            .package
            .as_ref()
            .map_or_else(String::new, PackageIdentity::key);
        format!("{package}::{}::{}", self.module, self.name)
    }
}
