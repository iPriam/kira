//! Process-lifetime storage for typed native callback state.

use std::collections::HashMap;

use std::sync::Arc;

use super::{
    NativeStateError, NativeStatePathStep, NativeStateToken, NativeStateTypeId, NativeStateValue,
    native_state_walk, native_state_walk_mut,
};

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    ty: NativeStateTypeId,
    value: NativeStateValue,
    /// Owners of this state: the Kira handle, every exported token, and every
    /// explicit retain. The value is destroyed by the release that takes this
    /// to zero, exactly once.
    refs: u64,
    /// Whether destroying `value` must happen under an executing Kira engine.
    requires_engine_release: bool,
}

/// A process-lifetime store of opaque, typed callback-state values.
#[derive(Debug, Default)]
pub struct NativeStateStore {
    next: u64,
    entries: HashMap<NativeStateToken, Entry>,
}

impl NativeStateStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self {
            next: Self::STRIDE,
            entries: HashMap::new(),
        }
    }

    /// The gap between two tokens this store hands out.
    ///
    /// Two, so every token it owns is even. A native engine keeps its state in
    /// a box and uses the box's own address as the token, with the low bit set
    /// to say so ([`NativeStateToken::is_boxed`]) — one bit tells the two
    /// apart, in a token space they share, without a lookup.
    const STRIDE: u64 = 2;

    /// Boxes an owned value and returns its stable non-zero token.
    pub fn create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.create_dropping(ty, value, None)
    }

    /// Boxes an owned value that runs a user `Drop` body, recording the body so
    /// the release that destroys the state can hand it back to be run.
    pub fn create_dropping(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
        glue: Option<u32>,
    ) -> Result<NativeStateToken, NativeStateError> {
        let value = Self::decorate_root_drop(value, glue)?;
        let requires_engine_release = value.requires_engine_release();
        let word = self.next;
        if word == 0 {
            return Err(NativeStateError::TokenExhausted);
        }
        self.next = self
            .next
            .checked_add(Self::STRIDE)
            .ok_or(NativeStateError::TokenExhausted)?;
        let token = NativeStateToken(word);
        self.entries.insert(
            token,
            Entry {
                ty,
                value,
                refs: 1,
                requires_engine_release,
            },
        );
        Ok(token)
    }

    /// Adds one owner to a live state.
    pub fn retain(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        let entry = self.entry_mut(token)?;
        entry.refs = entry
            .refs
            .checked_add(1)
            .ok_or(NativeStateError::TokenExhausted)?;
        Ok(())
    }

    /// Removes one owner from a live state, destroying it when none remain.
    ///
    /// Returns whether this release destroyed the state. Tokens are never
    /// reused, so a release after destruction is reported as an unknown token
    /// rather than reaching a state that took the same word later.
    pub fn release(&mut self, token: NativeStateToken) -> Result<bool, NativeStateError> {
        let (refs, requires_engine_release) = {
            let entry = self.entry(token)?;
            (entry.refs, entry.requires_engine_release)
        };
        if refs > 1 {
            self.entry_mut(token)?.refs -= 1;
            return Ok(false);
        }
        if requires_engine_release {
            return Err(NativeStateError::DropEngineRequired);
        }
        self.entries
            .remove(&token)
            .ok_or(NativeStateError::UnknownToken(token.as_word()))?;
        Ok(true)
    }

    /// Removes one owner and returns the owned value tree when the final release
    /// needs an executing Kira engine to destroy it. The tree itself carries all
    /// nested user `Drop` glue and affine NativeState ownership.
    pub fn release_dropping(
        &mut self,
        token: NativeStateToken,
    ) -> Result<Option<NativeStateValue>, NativeStateError> {
        let refs = self.entry(token)?.refs;
        if refs > 1 {
            self.entry_mut(token)?.refs -= 1;
            return Ok(None);
        }
        let entry = self
            .entries
            .remove(&token)
            .ok_or(NativeStateError::UnknownToken(token.as_word()))?;
        if entry.requires_engine_release {
            Ok(Some(entry.value))
        } else {
            Ok(None)
        }
    }

    /// How many states are live, whatever their owner counts.
    pub fn live(&self) -> usize {
        self.entries.len()
    }

    /// How many owners a live state has.
    pub fn owners(&self, token: NativeStateToken) -> Result<u64, NativeStateError> {
        Ok(self.entry(token)?.refs)
    }

    /// Returns an owned copy of the live value after validating its type.
    pub fn recover(
        &self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        let entry = self.entry(token)?;
        Self::check_type(entry.ty, requested)?;
        Ok(entry.value.clone())
    }

    /// Replaces the live value after validating its type, returning the displaced owner.
    pub fn replace(
        &mut self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateValue, NativeStateError> {
        let entry = self.entry_mut(token)?;
        Self::check_type(entry.ty, requested)?;
        entry.requires_engine_release = value.requires_engine_release();
        Ok(std::mem::replace(&mut entry.value, value))
    }

    /// Confirms a token names live state of `requested`, copying nothing.
    pub fn check(
        &self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        Self::check_type(self.entry(token)?.ty, requested)
    }

    /// Borrows what `path` addresses inside a live state.
    ///
    /// The whole point of addressing by path: reading one field of a state that
    /// also holds a glyph cache touches the field, not the cache.
    pub fn read_at(
        &self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<&NativeStateValue, NativeStateError> {
        let entry = self.entry(token)?;
        Self::check_type(entry.ty, requested)?;
        native_state_walk(&entry.value, path)
    }

    /// Replaces what `path` addresses and returns the displaced owned value.
    pub fn replace_at(
        &mut self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<NativeStateValue, NativeStateError> {
        let entry = self.entry_mut(token)?;
        Self::check_type(entry.ty, requested)?;
        let old = std::mem::replace(native_state_walk_mut(&mut entry.value, path)?, value);
        entry.requires_engine_release = entry.value.requires_engine_release();
        Ok(old)
    }

    /// Appends one owned value to the array at `path`.
    pub fn append_at(
        &mut self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        let entry = self.entry_mut(token)?;
        Self::check_type(entry.ty, requested)?;
        match native_state_walk_mut(&mut entry.value, path)? {
            NativeStateValue::Array(elements) => Arc::make_mut(elements).push(value),
            _ => return Err(NativeStateError::PathMismatch),
        }
        entry.requires_engine_release = entry.value.requires_engine_release();
        Ok(())
    }

    fn entry(&self, token: NativeStateToken) -> Result<&Entry, NativeStateError> {
        Self::check_non_null(token)?;
        self.entries
            .get(&token)
            .ok_or(NativeStateError::UnknownToken(token.as_word()))
    }

    fn entry_mut(&mut self, token: NativeStateToken) -> Result<&mut Entry, NativeStateError> {
        Self::check_non_null(token)?;
        self.entries
            .get_mut(&token)
            .ok_or(NativeStateError::UnknownToken(token.as_word()))
    }

    fn check_non_null(token: NativeStateToken) -> Result<(), NativeStateError> {
        if token.as_word() == 0 {
            Err(NativeStateError::NullToken)
        } else {
            Ok(())
        }
    }

    fn decorate_root_drop(
        value: NativeStateValue,
        glue: Option<u32>,
    ) -> Result<NativeStateValue, NativeStateError> {
        let Some(glue) = glue else {
            return Ok(value);
        };
        Ok(match value {
            NativeStateValue::Struct(fields) => NativeStateValue::DropStruct { glue, fields },
            value @ NativeStateValue::DropStruct { .. } => value,
            _ => return Err(NativeStateError::MalformedValue),
        })
    }

    fn check_type(
        actual: NativeStateTypeId,
        requested: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        if actual == requested {
            Ok(())
        } else {
            Err(NativeStateError::WrongType {
                actual: actual.as_word(),
                requested: requested.as_word(),
            })
        }
    }
}
