//! Collecting trait declarations and the conformances that claim them.
//!
//! Two passes, at two different moments. Traits are collected from syntax
//! alone, before any type table exists, because a trait declaration mentions no
//! type until its members are resolved — and because every later declaration
//! has to be able to say "that name is a trait". Conformances are collected
//! once every struct-shaped type has an id, because a conformance names one.

use std::collections::{HashMap, HashSet};

use kira_semantics_model::{StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ConstructKind, Function, Item, TraitRef, TypeRefId};

use super::{Conformance, Contract, SupertraitRef, TraitInfo, TraitMemberInfo, is_builtin_trait};
use crate::analyze::{Analyzer, Callable};

/// Every method name each type presents, keyed by the type.
type PresentedNames = HashMap<Type, HashSet<String>>;

mod claims;
mod declarations;
