//! What joining deferred work yields.
//!
//! An ordinary task handle is opaque, so the only thing its type has to carry
//! is the type of the value `.await` produces. The ordinary task surface keeps
//! that answer to two scalar cases. Main-thread work has the same handle shape
//! but can exchange every owned `Send` value, so it has a separate compact
//! descriptor for the non-recursive source types.
//!
//! Keeping it a small `Copy` enum is also what lets `Type::Task` sit inside
//! `Type` with no table behind it — a `Type` may not contain a `Type`.

/// The type `.await` yields for one task handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskResult {
    /// The body returns `Int`, or returns nothing — a `Void` task joins as `0`.
    Int,
    /// The body returns `Float`.
    Float,
}

impl TaskResult {
    /// How this result is spelled in a diagnostic.
    pub const fn label(self) -> &'static str {
        match self {
            TaskResult::Int => "Int",
            TaskResult::Float => "Float",
        }
    }
}

/// The source value a `MainThread.spawn` handle yields when it is awaited.
///
/// This is deliberately an indexed descriptor instead of `Type` itself:
/// putting `Type` inside `Type::MainThreadTask` would make the type enum
/// recursive. Every variant is a `Copy` type identity already present in the
/// program's type tables, so a handle remains a small, comparable value while
/// joins can recover the exact aggregate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MainThreadTaskResult {
    /// An integer, including a width spelling.
    Int(super::IntSpelling),
    /// A float, including a width spelling.
    Float(super::FloatSpelling),
    /// A boolean.
    Bool,
    /// A heap string.
    String,
    /// A declared struct.
    Struct(super::StructId),
    /// An array type.
    Array(super::ArrayId),
    /// A declared enum.
    Enum(super::EnumId),
    /// An opaque raw pointer word.
    RawPtr,
    /// A typed foreign pointer word.
    ForeignPtr(super::ForeignPtrId),
    /// An erased value.
    Any,
}

impl MainThreadTaskResult {
    /// Converts a source result type to its handle descriptor.
    pub fn from_type(ty: super::Type) -> Option<Self> {
        Some(match ty {
            super::Type::Void => Self::Int(super::IntSpelling::Plain),
            super::Type::Int(spelling) => Self::Int(spelling),
            super::Type::Float(spelling) => Self::Float(spelling),
            super::Type::Bool => Self::Bool,
            super::Type::String => Self::String,
            super::Type::Struct(id) => Self::Struct(id),
            super::Type::Array(id) => Self::Array(id),
            super::Type::Enum(id) => Self::Enum(id),
            super::Type::RawPtr => Self::RawPtr,
            super::Type::ForeignPtr(id) => Self::ForeignPtr(id),
            super::Type::Any => Self::Any,
            // A distinct type has no descriptor here, so a `MainThread.spawn`
            // of a function returning one is refused by name rather than
            // joined as the scalar underneath — awaiting it would hand back a
            // `U32` where a `TabId` was declared, which is the one thing the
            // type exists to prevent. Return `.raw` and rebuild after the join.
            super::Type::Distinct(_)
            | super::Type::Error
            | super::Type::Cell(_)
            | super::Type::CString
            | super::Type::CBlock
            | super::Type::NativeState(_)
            | super::Type::Task(_)
            | super::Type::MainThreadTask(_) => return None,
        })
    }

    /// Returns the value type produced by `.await`.
    pub const fn value_type(self) -> super::Type {
        match self {
            Self::Int(spelling) => super::Type::Int(spelling),
            Self::Float(spelling) => super::Type::Float(spelling),
            Self::Bool => super::Type::Bool,
            Self::String => super::Type::String,
            Self::Struct(id) => super::Type::Struct(id),
            Self::Array(id) => super::Type::Array(id),
            Self::Enum(id) => super::Type::Enum(id),
            Self::RawPtr => super::Type::RawPtr,
            Self::ForeignPtr(id) => super::Type::ForeignPtr(id),
            Self::Any => super::Type::Any,
        }
    }

    /// A compact name for diagnostics that do not own a type table.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Int(_) => "Int",
            Self::Float(_) => "Float",
            Self::Bool => "Bool",
            Self::String => "String",
            Self::Struct(_) => "Struct",
            Self::Array(_) => "Array",
            Self::Enum(_) => "Enum",
            Self::RawPtr | Self::ForeignPtr(_) => "RawPtr",
            Self::Any => "Any",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Type;

    #[test]
    fn a_task_type_is_distinct_per_result() {
        assert_ne!(Type::Task(TaskResult::Int), Type::Task(TaskResult::Float));
        assert_eq!(Type::Task(TaskResult::Int), Type::Task(TaskResult::Int));
    }

    #[test]
    fn a_task_handle_is_assignable_only_to_its_own_type() {
        let handle = Type::Task(TaskResult::Int);
        assert!(handle.assignable_to(handle));
        assert!(!handle.assignable_to(Type::INT));
        assert!(!Type::INT.assignable_to(handle));
    }

    #[test]
    fn a_main_thread_result_keeps_the_exact_value_type() {
        let result = MainThreadTaskResult::from_type(Type::String).expect("string result");
        assert_eq!(result.value_type(), Type::String);
        assert_eq!(result.label(), "String");
        assert_eq!(
            MainThreadTaskResult::from_type(Type::Void)
                .expect("void result")
                .value_type(),
            Type::INT
        );
    }
}
