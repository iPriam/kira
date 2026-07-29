//! What joining a deferred task yields.
//!
//! A task handle is opaque, so the only thing its type has to carry is the type
//! of the value `.await` produces. That is a two-case answer rather than an
//! arbitrary [`Type`](super::Type), because the executable slice restricts a
//! task body to a scalar-returning call: a `Void` body joins as `Int` `0`, so
//! `Int` and `Float` are the whole set.
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
}
