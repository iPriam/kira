//! Interned opaque callback-state handle types.

use std::collections::HashMap;

use super::Type;

/// The identity of one `NativeState<Value>` handle type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct NativeStateId(u32);

impl NativeStateId {
    /// Returns this id's zero-based table index.
    pub const fn index(self) -> u32 {
        self.0
    }
}

/// Every `NativeState<Value>` shape mentioned by a program.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NativeStateTable {
    targets: Vec<Type>,
    index: HashMap<Type, NativeStateId>,
}

impl NativeStateTable {
    /// Interns a handle for `target`, returning `None` when the id space is full.
    pub fn intern(&mut self, target: Type) -> Option<NativeStateId> {
        if let Some(id) = self.index.get(&target) {
            return Some(*id);
        }
        let id = NativeStateId(u32::try_from(self.targets.len()).ok()?);
        self.targets.push(target);
        self.index.insert(target, id);
        Some(id)
    }

    /// Returns the value type boxed by `id`.
    pub fn target(&self, id: NativeStateId) -> Option<Type> {
        self.targets.get(id.0 as usize).copied()
    }

    /// Rewrites every row's boxed type through `visit`, then rebuilds the
    /// intern index. See [`super::arrays::ArrayTable::visit_elements_mut`] for
    /// why a duplicate row is kept rather than merged.
    pub fn visit_targets_mut(&mut self, visit: &dyn Fn(&mut Type)) {
        for target in &mut self.targets {
            visit(target);
        }
        self.index.clear();
        for (index, target) in self.targets.iter().enumerate() {
            self.index
                .entry(*target)
                .or_insert(NativeStateId(index as u32));
        }
    }
}
