//! Portable callback-state values, tokens, stores, and host errors.

use std::sync::Arc;

use thiserror::Error;

use crate::{
    FileRequest, FileResponse, FileSystemError, ForeignArg, ForeignCallError, ForeignResult,
    HostCapabilities, LinuxSyscall, NativeArg, NativeCallError, NativeReturn, SyscallError,
};

mod cblock;
mod owner;

pub use cblock::{CBlockOffset, NativeCBlock, NativeCBlockChild, NativeCBlockError};
pub use owner::NativeStateOwner;

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

    /// Whether this token names a boxed state rather than a stored value.
    ///
    /// A native engine holds state the way Rust does — one allocation, the
    /// value in it, fields addressed directly — and uses the box's address as
    /// the token. A box is at least two-byte aligned, so the low bit is free to
    /// mark one, and [`NativeStateStore`] hands out only even tokens. Nothing
    /// has to look a token up to know which kind it is.
    pub const fn is_boxed(self) -> bool {
        self.0 & 1 == 1
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
    /// Capture-cell share node.
    pub const CELL: Self = Self(9);
    /// A dynamically typed `Any` node: its type identity and one payload child.
    pub const ANY: Self = Self(10);
    /// A uniquely owned C-storage block node; see [`NativeStateValue::CBlock`].
    pub const C_BLOCK: Self = Self(11);
    /// One tracked callback-state ownership obligation nested in another value.
    pub const NATIVE_STATE: Self = Self(12);
    /// A struct aggregate whose own user `Drop` body must run before its fields release.
    pub const DROP_STRUCT: Self = Self(13);
    /// An exact base-10 decimal (`Number`): a two-word inline value.
    pub const NUMBER: Self = Self(14);
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
    /// The last owner requires an executing Kira engine to run destruction.
    pub const DROP_ENGINE_REQUIRED: Self = Self(7);
}

impl From<NativeStateError> for NativeStateStatus {
    fn from(error: NativeStateError) -> Self {
        match error {
            NativeStateError::NoStateHost => Self::NO_HOST,
            NativeStateError::NullToken => Self::NULL_TOKEN,
            NativeStateError::UnknownToken(_) => Self::UNKNOWN_TOKEN,
            NativeStateError::WrongType { .. } => Self::WRONG_TYPE,
            NativeStateError::TokenExhausted => Self::TOKEN_EXHAUSTED,
            NativeStateError::DropEngineRequired => Self::DROP_ENGINE_REQUIRED,
            // A path that addresses nothing and a malformed node are the same
            // status on the wire: both say the stored value did not have the
            // shape the caller read it as, which is the whole of what a C caller
            // can act on. No new status code, so no wire change.
            NativeStateError::MalformedValue | NativeStateError::PathMismatch => {
                Self::MALFORMED_VALUE
            }
        }
    }
}

/// One share of a capture cell, held by a callback-state tree.
///
/// # Why a cell is a share rather than a copy
///
/// Every other node in a state tree is a *copy* of what the engine held: the
/// value moved in, and nothing on the engine's side can still see it. A capture
/// cell is the one Kira value with reference semantics — a closure and the frame
/// that declared the `var` have to see each other's writes — so copying its
/// contents into the tree would silently split one binding into two.
///
/// So the node holds the engine's own cell, by handle, with a share taken for
/// it. The share is counted *here*, by the [`Arc`]: a tree node is cloned
/// whenever [`native_state_walk_mut`] unshares the level above it, and an engine
/// asked to count those clones would need a hook on every one. Counting them
/// with an `Arc` makes the arithmetic exact by construction, and the engine
/// hears exactly once — when the last share in the tree goes — through the
/// release it supplied.
#[derive(Debug, Clone)]
pub struct NativeCell {
    share: Arc<CellShare>,
}

/// The share itself, so that dropping the last clone releases it once.
struct CellShare {
    handle: u64,
    vm_owned: bool,
    release: Box<dyn Fn(u64) + Send + Sync>,
}

impl std::fmt::Debug for CellShare {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CellShare")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl Drop for CellShare {
    fn drop(&mut self) {
        (self.release)(self.handle);
    }
}

impl NativeCell {
    /// Takes over one already-retained share of the cell `handle` names.
    ///
    /// The caller retains; this releases. An engine that handed over a share it
    /// had not taken would see the count fall below what it lent out.
    pub fn new(handle: u64, release: impl Fn(u64) + Send + Sync + 'static) -> Self {
        Self::with_origin(handle, false, release)
    }

    /// Takes over one VM-owned share of the cell `handle` names.
    pub fn from_vm(handle: u64, release: impl Fn(u64) + Send + Sync + 'static) -> Self {
        Self::with_origin(handle, true, release)
    }

    fn with_origin(
        handle: u64,
        vm_owned: bool,
        release: impl Fn(u64) + Send + Sync + 'static,
    ) -> Self {
        Self {
            share: Arc::new(CellShare {
                handle,
                vm_owned,
                release: Box::new(release),
            }),
        }
    }

    /// The engine handle this shares.
    pub fn handle(&self) -> u64 {
        self.share.handle
    }

    /// Whether this handle names a VM cell rather than a native cell.
    pub fn is_vm_owned(&self) -> bool {
        self.share.vm_owned
    }
}

/// Two shares are the same cell when they name the same storage.
///
/// Identity, not contents: a cell is a place to write, and two boxes holding
/// equal values are still two places. The same rule the VM's `Heap` and the
/// native backend's `icmp eq` apply to a cell.
impl PartialEq for NativeCell {
    fn eq(&self, other: &Self) -> bool {
        self.share.handle == other.share.handle
    }
}

/// An owned, backend-neutral copy of a Kira value held as callback state.
///
/// # An aggregate is shared until somebody writes to it
///
/// Every aggregate node holds its children behind an [`Arc`], so cloning one is
/// a refcount bump rather than a walk of everything underneath it. That is what
/// makes reading a field out of live state cost the field: the read hands back a
/// node that shares its children with the stored one, and only a *write* through
/// [`native_state_walk_mut`] gives the writer children of its own — one
/// [`Arc::make_mut`] per level of the path, once, after which the path is
/// unshared and every later write through it is a compare.
///
/// This is the same bargain [`crate`]'s arrays strike on both engines: share the
/// block, make it unique on the first write. It matters here because the shared
/// node is also a *snapshot* — a reader holding one keeps seeing what it read
/// even if the stored value is written afterwards, which is exactly the value
/// semantics a Kira read has.
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
    /// An exact base-10 decimal (`Number`), an inline two-word value.
    Number(crate::Decimal),
    /// A Kira struct's fields in declaration order, shared until written to.
    Struct(Arc<Vec<NativeStateValue>>),
    /// A Kira struct that runs `glue` before releasing its fields.
    DropStruct {
        /// The function index of the struct's user `Drop` body.
        glue: u32,
        /// Fields in declaration order, shared until written to.
        fields: Arc<Vec<NativeStateValue>>,
    },
    /// A Kira array's elements in index order, shared until written to.
    Array(Arc<Vec<NativeStateValue>>),
    /// An opaque raw-pointer word.
    RawPtr(u64),
    /// A share of a capture cell the engine still owns the storage of.
    Cell(NativeCell),
    /// An erased value, retaining the type identity that `Any` carries.
    Any {
        /// The [`kira_semantics_model::ErasedTypeId`] word of the value before
        /// it entered `Any`.
        type_id: u64,
        /// The value that was erased, represented recursively as a state node.
        payload: Arc<NativeStateValue>,
    },
    /// A Kira enum's tag and optional payload.
    Enum {
        /// The declaration-order variant tag.
        tag: u32,
        /// The selected variant's payload, when it has one, shared until
        /// written to.
        payload: Option<Arc<NativeStateValue>>,
    },
    /// One affine `NativeState<T>` owner nested in another callback-state value.
    /// Transport-tree clones share this one obligation; materializing another
    /// Kira owner uses [`NativeStateOwner::duplicate_owner`].
    NativeState(NativeStateOwner),
    /// A uniquely owned block of C storage crossing between engines.
    ///
    /// The bytes move with the node; whichever engine absorbs it materializes
    /// a block it owns, so the payload address C reads is always inside the
    /// engine currently holding the value. See
    /// [`crate::c_storage`] for the ownership contract.
    CBlock(NativeCBlock),
}

impl NativeStateValue {
    /// A struct node owning `fields`.
    pub fn struct_of(fields: Vec<NativeStateValue>) -> NativeStateValue {
        NativeStateValue::Struct(Arc::new(fields))
    }

    /// A struct node whose own user `Drop` body is `glue`.
    pub fn dropping_struct_of(fields: Vec<NativeStateValue>, glue: u32) -> NativeStateValue {
        NativeStateValue::DropStruct {
            glue,
            fields: Arc::new(fields),
        }
    }

    /// An array node owning `elements`.
    pub fn array_of(elements: Vec<NativeStateValue>) -> NativeStateValue {
        NativeStateValue::Array(Arc::new(elements))
    }

    /// An enum node with `tag` and an optional owned `payload`.
    pub fn enum_of(tag: u32, payload: Option<NativeStateValue>) -> NativeStateValue {
        NativeStateValue::Enum {
            tag,
            payload: payload.map(Arc::new),
        }
    }

    /// An erased value node owning its dynamic payload.
    pub fn any_of(type_id: u64, payload: NativeStateValue) -> NativeStateValue {
        NativeStateValue::Any {
            type_id,
            payload: Arc::new(payload),
        }
    }

    /// This aggregate's children as a slice, or `None` for a scalar.
    ///
    /// A struct and an array answer with their fields and elements; an enum
    /// answers with its payload, which is why the caller gets a slice rather
    /// than one of the three shapes.
    pub fn children(&self) -> Option<&[NativeStateValue]> {
        match self {
            NativeStateValue::Struct(values) | NativeStateValue::Array(values) => Some(values),
            NativeStateValue::DropStruct { fields, .. } => Some(fields),
            NativeStateValue::Enum { payload, .. } => {
                Some(payload.as_deref().map_or(&[], std::slice::from_ref))
            }
            NativeStateValue::Any { payload, .. } => Some(std::slice::from_ref(payload.as_ref())),
            _ => None,
        }
    }

    /// Takes this aggregate's children, cloning them only if they are shared.
    ///
    /// The unshared case is the common one — a node built to be taken apart —
    /// and it moves rather than copies.
    pub fn into_children(self) -> Option<Vec<NativeStateValue>> {
        match self {
            NativeStateValue::Struct(values) | NativeStateValue::Array(values) => {
                Some(unwrap_children(values))
            }
            NativeStateValue::DropStruct { fields, .. } => Some(unwrap_children(fields)),
            NativeStateValue::Enum { payload, .. } => Some(match payload {
                Some(payload) => {
                    vec![Arc::try_unwrap(payload).unwrap_or_else(|arc| (*arc).clone())]
                }
                None => Vec::new(),
            }),
            NativeStateValue::Any { payload, .. } => Some(vec![
                Arc::try_unwrap(payload).unwrap_or_else(|arc| (*arc).clone()),
            ]),
            _ => None,
        }
    }

    /// Whether destroying this transport tree may need to enter Kira code.
    ///
    /// A `DropStruct` runs a user body directly. A nested `NativeState` may own
    /// another state whose final release runs user code, so it is conservatively
    /// returned to the active engine as well. Aggregates inherit the obligation
    /// from any child they own.
    pub fn requires_engine_release(&self) -> bool {
        match self {
            NativeStateValue::DropStruct { .. } | NativeStateValue::NativeState(_) => true,
            NativeStateValue::Struct(values) | NativeStateValue::Array(values) => {
                values.iter().any(NativeStateValue::requires_engine_release)
            }
            NativeStateValue::Enum { payload, .. } => payload
                .as_deref()
                .is_some_and(NativeStateValue::requires_engine_release),
            NativeStateValue::Any { payload, .. } => payload.requires_engine_release(),
            _ => false,
        }
    }
}

/// Takes a shared child list over, copying it only when it is shared.
fn unwrap_children(values: Arc<Vec<NativeStateValue>>) -> Vec<NativeStateValue> {
    Arc::try_unwrap(values).unwrap_or_else(|arc| (*arc).clone())
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
    /// Destroying the last owner would need to enter Kira code.
    #[error("native callback-state destruction requires an executing Kira engine")]
    DropEngineRequired,
    /// A path addressed something the stored value does not have there.
    ///
    /// The compiler resolves every field and index against a checked type, so
    /// this surfaces state whose stored shape disagrees with the program
    /// reading it — never a program that merely type-checked.
    #[error("native callback-state path does not address a value of that shape")]
    PathMismatch,
}

/// One step down into a stored callback-state value.
///
/// Field indices and array indices are distinct steps rather than one integer:
/// a struct and an array are both indexed sequences in storage, and conflating
/// them would let a path read a struct's third field as an array element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeStatePathStep {
    /// The field at this declaration-order index.
    Field(u32),
    /// The element at this index.
    Index(u64),
}

impl NativeStatePathStep {
    /// Wire tag for a declaration-order struct field.
    pub const FIELD_WIRE_TAG: u8 = 0;
    /// Wire tag for an array index.
    pub const INDEX_WIRE_TAG: u8 = 1;

    /// Encodes one step for the native callback-state path ABI.
    pub const fn to_wire(self) -> (u8, u64) {
        match self {
            Self::Field(index) => (Self::FIELD_WIRE_TAG, index as u64),
            Self::Index(index) => (Self::INDEX_WIRE_TAG, index),
        }
    }

    /// Decodes one step from the native callback-state path ABI.
    pub fn from_wire(tag: u8, value: u64) -> Result<Self, NativeStateError> {
        match tag {
            Self::FIELD_WIRE_TAG => u32::try_from(value)
                .map(Self::Field)
                .map_err(|_| NativeStateError::PathMismatch),
            Self::INDEX_WIRE_TAG => Ok(Self::Index(value)),
            _ => Err(NativeStateError::PathMismatch),
        }
    }
}

/// Follows `path` into a stored value, borrowing what it addresses.
pub fn native_state_walk<'a>(
    root: &'a NativeStateValue,
    path: &[NativeStatePathStep],
) -> Result<&'a NativeStateValue, NativeStateError> {
    let mut cursor = root;
    for step in path {
        cursor = match (step, cursor) {
            (NativeStatePathStep::Field(index), NativeStateValue::Struct(fields))
            | (NativeStatePathStep::Field(index), NativeStateValue::DropStruct { fields, .. }) => {
                fields
                    .get(*index as usize)
                    .ok_or(NativeStateError::PathMismatch)?
            }
            (NativeStatePathStep::Index(index), NativeStateValue::Array(elements)) => elements
                .get(usize::try_from(*index).map_err(|_| NativeStateError::PathMismatch)?)
                .ok_or(NativeStateError::PathMismatch)?,
            _ => return Err(NativeStateError::PathMismatch),
        };
    }
    Ok(cursor)
}

/// Follows `path` into a stored value, borrowing what it addresses mutably.
///
/// Every level the walk passes through is made unique on the way down: the
/// children are shared with whoever else read this node, and a write must not
/// land in their copy. That is one [`Arc::make_mut`] per level of the path, and
/// only while the level is actually shared — a path walked twice unshares on the
/// first walk and compares on the second.
pub fn native_state_walk_mut<'a>(
    root: &'a mut NativeStateValue,
    path: &[NativeStatePathStep],
) -> Result<&'a mut NativeStateValue, NativeStateError> {
    let mut cursor = root;
    for step in path {
        cursor = match (step, cursor) {
            (NativeStatePathStep::Field(index), NativeStateValue::Struct(fields)) => {
                Arc::make_mut(fields)
                    .get_mut(*index as usize)
                    .ok_or(NativeStateError::PathMismatch)?
            }
            (NativeStatePathStep::Field(index), NativeStateValue::DropStruct { fields, .. }) => {
                Arc::make_mut(fields)
                    .get_mut(*index as usize)
                    .ok_or(NativeStateError::PathMismatch)?
            }
            (NativeStatePathStep::Index(index), NativeStateValue::Array(elements)) => {
                Arc::make_mut(elements)
                    .get_mut(usize::try_from(*index).map_err(|_| NativeStateError::PathMismatch)?)
                    .ok_or(NativeStateError::PathMismatch)?
            }
            _ => return Err(NativeStateError::PathMismatch),
        };
    }
    Ok(cursor)
}

mod host;
mod store;

pub use host::NativeStateHost;
pub use store::NativeStateStore;

#[cfg(test)]
mod tests;
