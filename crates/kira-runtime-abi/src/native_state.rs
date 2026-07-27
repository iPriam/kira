//! Portable callback-state values, tokens, stores, and host errors.

use std::collections::HashMap;

use thiserror::Error;

use crate::{
    FileRequest, FileResponse, FileSystemError, ForeignArg, ForeignCallError, ForeignResult,
    HostCapabilities, NativeArg, NativeCallError, NativeResult,
};

/// The program-stable identity of a type stored in native callback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct NativeStateTypeId(u64);

impl NativeStateTypeId {
    /// Creates an id from the compiler's program-stable word.
    pub const fn new(word: u64) -> Self {
        Self(word)
    }

    /// Returns the word carried through bytecode and the native runtime ABI.
    pub const fn as_word(self) -> u64 {
        self.0
    }
}

/// A stable opaque token native code may store and return as userdata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct NativeStateToken(u64);

impl NativeStateToken {
    /// Reconstructs a token from an opaque userdata word.
    pub const fn from_word(word: u64) -> Self {
        Self(word)
    }

    /// Returns the opaque userdata word.
    pub const fn as_word(self) -> u64 {
        self.0
    }
}

/// The open C tag of a backend-neutral callback-state value node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct NativeStateValueTag(pub u32);

impl NativeStateValueTag {
    /// Integer node.
    pub const INT: Self = Self(1);
    /// Floating-point node.
    pub const FLOAT: Self = Self(2);
    /// Boolean node.
    pub const BOOL: Self = Self(3);
    /// String node.
    pub const STRING: Self = Self(4);
    /// Struct aggregate node.
    pub const STRUCT: Self = Self(5);
    /// Array aggregate node.
    pub const ARRAY: Self = Self(6);
    /// Enum aggregate node.
    pub const ENUM: Self = Self(7);
    /// Opaque raw-pointer word node.
    pub const RAW_PTR: Self = Self(8);
}

/// The open C status returned by native callback-state runtime helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct NativeStateStatus(pub u32);

impl NativeStateStatus {
    /// Operation succeeded.
    pub const OK: Self = Self(0);
    /// No state host exists.
    pub const NO_HOST: Self = Self(1);
    /// The token was null.
    pub const NULL_TOKEN: Self = Self(2);
    /// The token was unknown or already freed.
    pub const UNKNOWN_TOKEN: Self = Self(3);
    /// The requested type did not match the boxed type.
    pub const WRONG_TYPE: Self = Self(4);
    /// Token allocation exhausted its id space.
    pub const TOKEN_EXHAUSTED: Self = Self(5);
    /// A value node was malformed or had the wrong shape.
    pub const MALFORMED_VALUE: Self = Self(6);
}

impl From<NativeStateError> for NativeStateStatus {
    fn from(error: NativeStateError) -> Self {
        match error {
            NativeStateError::NoStateHost => Self::NO_HOST,
            NativeStateError::NullToken => Self::NULL_TOKEN,
            NativeStateError::UnknownToken(_) => Self::UNKNOWN_TOKEN,
            NativeStateError::WrongType { .. } => Self::WRONG_TYPE,
            NativeStateError::TokenExhausted => Self::TOKEN_EXHAUSTED,
            NativeStateError::MalformedValue => Self::MALFORMED_VALUE,
        }
    }
}

/// An owned, backend-neutral copy of a Kira value held as callback state.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeStateValue {
    /// A Kira integer value.
    Int(i64),
    /// A Kira floating-point value.
    Float(f64),
    /// A Kira boolean value.
    Bool(bool),
    /// A Kira string value.
    String(String),
    /// A Kira struct's fields in declaration order.
    Struct(Vec<NativeStateValue>),
    /// A Kira array's elements in index order.
    Array(Vec<NativeStateValue>),
    /// An opaque raw-pointer word.
    RawPtr(u64),
    /// A Kira enum's tag and optional payload.
    Enum {
        /// The declaration-order variant tag.
        tag: u32,
        /// The selected variant's payload, when it has one.
        payload: Option<Box<NativeStateValue>>,
    },
}

/// A deterministic failure from the opaque callback-state store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NativeStateError {
    /// The host does not provide callback-state storage.
    #[error("this host does not provide native callback-state storage")]
    NoStateHost,
    /// The null userdata word does not name state.
    #[error("native callback-state token is null")]
    NullToken,
    /// No live state was allocated with this token.
    #[error("native callback-state token {0} is unknown or was already freed")]
    UnknownToken(u64),
    /// The requested recovery type differs from the boxed type.
    #[error("native callback-state type mismatch: boxed type {actual}, requested type {requested}")]
    WrongType {
        /// The type recorded when the state was boxed.
        actual: u64,
        /// The type requested by recovery or replacement.
        requested: u64,
    },
    /// The process exhausted the non-zero token space.
    #[error("native callback-state token space is exhausted")]
    TokenExhausted,
    /// A backend-neutral value node was malformed.
    #[error("native callback-state value is malformed")]
    MalformedValue,
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    ty: NativeStateTypeId,
    value: NativeStateValue,
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
            next: 1,
            entries: HashMap::new(),
        }
    }

    /// Boxes an owned value and returns its stable non-zero token.
    pub fn create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        let word = self.next;
        if word == 0 {
            return Err(NativeStateError::TokenExhausted);
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or(NativeStateError::TokenExhausted)?;
        let token = NativeStateToken(word);
        self.entries.insert(token, Entry { ty, value });
        Ok(token)
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

    /// Replaces the live value after validating its type.
    pub fn replace(
        &mut self,
        token: NativeStateToken,
        requested: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        let entry = self.entry_mut(token)?;
        Self::check_type(entry.ty, requested)?;
        entry.value = value;
        Ok(())
    }

    /// Releases one state exactly once.
    pub fn free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        Self::check_non_null(token)?;
        self.entries
            .remove(&token)
            .map(|_| ())
            .ok_or(NativeStateError::UnknownToken(token.as_word()))
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

/// A host wrapper that adds portable native callback-state storage.
#[derive(Debug)]
pub struct NativeStateHost<H> {
    inner: H,
    store: NativeStateStore,
}

impl<H> NativeStateHost<H> {
    /// Wraps `inner` with an empty callback-state store.
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            store: NativeStateStore::new(),
        }
    }

    /// Borrows the wrapped host.
    pub fn inner(&self) -> &H {
        &self.inner
    }

    /// Mutably borrows the wrapped host.
    pub fn inner_mut(&mut self) -> &mut H {
        &mut self.inner
    }

    /// Returns the wrapped host.
    pub fn into_inner(self) -> H {
        self.inner
    }
}

impl<H: HostCapabilities> HostCapabilities for NativeStateHost<H> {
    fn write_line(&mut self, text: &str) {
        self.inner.write_line(text);
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, NativeCallError> {
        self.inner.call_native(function_id, args)
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        self.inner.call_foreign(foreign_id, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.inner.foreign_callback(callback_id)
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.store.create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.store.recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.store.replace(token, ty, value)
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.store.free(token)
    }

    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        self.inner.file_system(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: NativeStateTypeId = NativeStateTypeId::new(1);
    const RIGHT: NativeStateTypeId = NativeStateTypeId::new(2);

    #[test]
    fn native_state_c_tags_and_statuses_are_pinned() {
        assert_eq!(NativeStateValueTag::INT.0, 1);
        assert_eq!(NativeStateValueTag::FLOAT.0, 2);
        assert_eq!(NativeStateValueTag::BOOL.0, 3);
        assert_eq!(NativeStateValueTag::STRING.0, 4);
        assert_eq!(NativeStateValueTag::STRUCT.0, 5);
        assert_eq!(NativeStateValueTag::ARRAY.0, 6);
        assert_eq!(NativeStateValueTag::ENUM.0, 7);
        assert_eq!(NativeStateValueTag::RAW_PTR.0, 8);
        assert_eq!(NativeStateStatus::OK.0, 0);
        assert_eq!(NativeStateStatus::NO_HOST.0, 1);
        assert_eq!(NativeStateStatus::NULL_TOKEN.0, 2);
        assert_eq!(NativeStateStatus::UNKNOWN_TOKEN.0, 3);
        assert_eq!(NativeStateStatus::WRONG_TYPE.0, 4);
        assert_eq!(NativeStateStatus::TOKEN_EXHAUSTED.0, 5);
        assert_eq!(NativeStateStatus::MALFORMED_VALUE.0, 6);
    }

    #[test]
    fn store_recovers_mutates_and_frees_once() {
        let mut store = NativeStateStore::new();
        let token = store
            .create(
                LEFT,
                NativeStateValue::Struct(vec![NativeStateValue::Int(3)]),
            )
            .expect("state allocates");
        assert_ne!(token.as_word(), 0);
        assert_eq!(
            store.recover(token, LEFT),
            Ok(NativeStateValue::Struct(vec![NativeStateValue::Int(3)]))
        );
        store
            .replace(
                token,
                LEFT,
                NativeStateValue::Struct(vec![NativeStateValue::Int(7)]),
            )
            .expect("state mutates");
        assert_eq!(
            store.recover(token, LEFT),
            Ok(NativeStateValue::Struct(vec![NativeStateValue::Int(7)]))
        );
        assert_eq!(store.free(token), Ok(()));
        assert_eq!(
            store.free(token),
            Err(NativeStateError::UnknownToken(token.as_word()))
        );
    }

    #[test]
    fn store_rejects_wrong_null_and_unknown_tokens() {
        let mut store = NativeStateStore::new();
        let token = store
            .create(LEFT, NativeStateValue::Int(1))
            .expect("state allocates");
        assert_eq!(
            store.recover(token, RIGHT),
            Err(NativeStateError::WrongType {
                actual: LEFT.as_word(),
                requested: RIGHT.as_word(),
            })
        );
        assert_eq!(
            store.recover(NativeStateToken::from_word(0), LEFT),
            Err(NativeStateError::NullToken)
        );
        assert_eq!(
            store.free(NativeStateToken::from_word(999)),
            Err(NativeStateError::UnknownToken(999))
        );
    }
}
