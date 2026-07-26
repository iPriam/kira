//! Compiler-recognized file-system intrinsics.
//!
//! These are the primitives bundled Foundation's `FileSystem` surface is built
//! on. They are intrinsics rather than ordinary functions for the same reason
//! `print` is: reaching the outside world is an effect the engine performs, not
//! something Kira code can express, and each engine performs it its own way —
//! the VM through its host, native code through `kira_rt_fs_*`.
//!
//! Each one has one fixed signature. There is no overloading and no inference:
//! an argument either has the declared type or the call is refused, which keeps
//! the instruction's operand types knowable at compile time on every backend.

use kira_runtime_abi::FileSystemOp;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{IntSpelling, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Analyzes one file-system intrinsic, or returns `None` for another name.
    pub(super) fn analyze_file_system_intrinsic(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        let op = FileSystemOp::from_intrinsic_name(name)?;
        self.reject_intrinsic_type_args(name, type_args, span);

        let values: Vec<HirExprId> = args
            .iter()
            .map(|arg| self.analyze_expr(ctx, arg.value))
            .collect();
        let expected = self.file_system_parameters(op);
        if values.len() != expected.len() {
            self.emit(
                span,
                "KSEM252",
                format!(
                    "`{name}` takes exactly {} argument{}, found {}",
                    expected.len(),
                    if expected.len() == 1 { "" } else { "s" },
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let mut refused = false;
        for (index, (&value, &want)) in values.iter().zip(expected.iter()).enumerate() {
            let got = self.program.expr(value).type_of();
            if !got.assignable_to(want) {
                self.emit(
                    span,
                    "KSEM253",
                    format!(
                        "argument {} of `{name}` expects `{}`, found `{}`",
                        index + 1,
                        self.type_name(want),
                        self.type_name(got)
                    ),
                );
                refused = true;
            }
        }
        if refused {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let ty = self.file_system_result(op);
        Some(self.program.exprs.alloc(HirExpr::FileSystem {
            op,
            args: values,
            ty,
        }))
    }

    /// The parameter types of one intrinsic, in order.
    fn file_system_parameters(&mut self, op: FileSystemOp) -> Vec<Type> {
        let bytes = self.byte_array();
        match op {
            FileSystemOp::ReadRange => vec![Type::String, Type::INT, Type::INT],
            FileSystemOp::WriteBytes => vec![Type::String, bytes],
            FileSystemOp::WriteText | FileSystemOp::RenamePath => {
                vec![Type::String, Type::String]
            }
            FileSystemOp::ReadText
            | FileSystemOp::ListDirectory
            | FileSystemOp::IsDirectory
            | FileSystemOp::MakeDirectory
            | FileSystemOp::RemovePath
            | FileSystemOp::FileExists
            | FileSystemOp::PathExists
            | FileSystemOp::FileSize => vec![Type::String],
        }
    }

    /// The result type of one intrinsic.
    fn file_system_result(&mut self, op: FileSystemOp) -> Type {
        match op {
            FileSystemOp::ReadRange => self.byte_array(),
            FileSystemOp::ReadText => Type::String,
            FileSystemOp::ListDirectory => self.program.types.array_of(Type::String),
            FileSystemOp::FileSize => Type::Int(IntSpelling::U64),
            FileSystemOp::WriteBytes
            | FileSystemOp::WriteText
            | FileSystemOp::IsDirectory
            | FileSystemOp::MakeDirectory
            | FileSystemOp::RenamePath
            | FileSystemOp::RemovePath
            | FileSystemOp::FileExists
            | FileSystemOp::PathExists => Type::Bool,
        }
    }

    /// The interned `[U8]` type both byte-level operations speak.
    fn byte_array(&mut self) -> Type {
        self.program.types.array_of(Type::Int(IntSpelling::U8))
    }
}
