//! What joining deferred work yields.
//!
//! A task handle — an ordinary [`Type::Task`] or a [`Type::MainThreadTask`] —
//! is opaque, so the only thing its type has to carry is the type of the value
//! `.await` produces. Both handles yield an owned `Send` value, exactly the set
//! a channel carries, so one descriptor answers for both.
//!
//! [`Type`]: super::Type
//! [`Type::Task`]: super::Type::Task
//! [`Type::MainThreadTask`]: super::Type::MainThreadTask
//!
//! This is deliberately an indexed descriptor instead of [`Type`] itself:
//! putting a `Type` inside `Type::Task` would make the type enum recursive.
//! Every variant is a `Copy` type identity already present in the program's
//! type tables, so a handle stays a small, comparable value while a join can
//! recover the exact source type.

/// The type `.await` yields for one task handle.
///
/// One descriptor for both handle shapes: the two `Type` variants stay distinct
/// (a task handle and a main-thread handle are different types), but what either
/// one yields on `.await` is described the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskResult {
    /// An integer, including a width spelling. A `Void` body joins here as `0`.
    Int(super::IntSpelling),
    /// A float, including a width spelling.
    Float(super::FloatSpelling),
    /// A boolean.
    Bool,
    /// A heap string.
    String,
    /// A heap decimal.
    Number,
    /// A declared struct.
    Struct(super::StructId),
    /// An array type.
    Array(super::ArrayId),
    /// A declared enum.
    Enum(super::EnumId),
    /// A `distinct` type, kept by identity so a join hands back the exact type
    /// the body returned rather than the scalar underneath it.
    Distinct(super::DistinctId),
    /// An opaque raw pointer word.
    RawPtr,
    /// A typed foreign pointer word.
    ForeignPtr(super::ForeignPtrId),
    /// An erased value.
    Any,
}

impl TaskResult {
    /// Converts a source result type to its handle descriptor, or `None` for a
    /// type no handle yields.
    pub fn from_type(ty: super::Type) -> Option<Self> {
        Some(match ty {
            super::Type::Void => Self::Int(super::IntSpelling::Plain),
            super::Type::Int(spelling) => Self::Int(spelling),
            super::Type::Float(spelling) => Self::Float(spelling),
            super::Type::Bool => Self::Bool,
            super::Type::String => Self::String,
            super::Type::Number => Self::Number,
            super::Type::Struct(id) => Self::Struct(id),
            super::Type::Array(id) => Self::Array(id),
            super::Type::Enum(id) => Self::Enum(id),
            super::Type::Distinct(id) => Self::Distinct(id),
            super::Type::RawPtr => Self::RawPtr,
            super::Type::ForeignPtr(id) => Self::ForeignPtr(id),
            super::Type::Any => Self::Any,
            super::Type::Error
            | super::Type::Cell(_)
            | super::Type::CString
            | super::Type::CBlock
            | super::Type::NativeState(_)
            | super::Type::Task(_)
            | super::Type::MainThreadTask(_)
            | super::Type::RuntimeType => return None,
        })
    }

    /// Returns the value type produced by `.await`.
    pub const fn value_type(self) -> super::Type {
        match self {
            Self::Int(spelling) => super::Type::Int(spelling),
            Self::Float(spelling) => super::Type::Float(spelling),
            Self::Bool => super::Type::Bool,
            Self::String => super::Type::String,
            Self::Number => super::Type::Number,
            Self::Struct(id) => super::Type::Struct(id),
            Self::Array(id) => super::Type::Array(id),
            Self::Enum(id) => super::Type::Enum(id),
            Self::Distinct(id) => super::Type::Distinct(id),
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
            Self::Number => "Number",
            Self::Struct(_) => "Struct",
            Self::Array(_) => "Array",
            Self::Enum(_) => "Enum",
            Self::Distinct(_) => "Distinct",
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
        assert_ne!(
            Type::Task(TaskResult::from_type(Type::INT).expect("int")),
            Type::Task(TaskResult::from_type(Type::FLOAT).expect("float"))
        );
        assert_eq!(
            Type::Task(TaskResult::from_type(Type::INT).expect("int")),
            Type::Task(TaskResult::from_type(Type::INT).expect("int"))
        );
    }

    #[test]
    fn a_task_handle_is_assignable_only_to_its_own_type() {
        let handle = Type::Task(TaskResult::from_type(Type::INT).expect("int"));
        assert!(handle.assignable_to(handle));
        assert!(!handle.assignable_to(Type::INT));
        assert!(!Type::INT.assignable_to(handle));
    }

    #[test]
    fn a_result_keeps_the_exact_value_type() {
        let result = TaskResult::from_type(Type::String).expect("string result");
        assert_eq!(result.value_type(), Type::String);
        assert_eq!(result.label(), "String");
        assert_eq!(
            TaskResult::from_type(Type::Void)
                .expect("void result")
                .value_type(),
            Type::INT
        );
    }
}
