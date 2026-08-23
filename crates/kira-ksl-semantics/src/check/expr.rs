//! Expressions: what each one computes, and what type that has.
//!
//! # Literals carry into their context
//!
//! `let count: UInt = 255` and `position.x * 0.25` both work because a
//! *literal* — and only a literal — retypes to what its position expects.
//! Anything else must already match. That is the smallest rule that accepts
//! the corpus without letting a silent `Float`-to-`UInt` conversion through
//! somewhere a shader did not ask for one.

use kira_ksl_syntax_model::ast::{BinaryOp as SyntaxBinaryOp, Expr, UnaryOp as SyntaxUnaryOp};
use kira_ksl_syntax_model::tree::ExprId;
use kira_shader_model::{ScalarType, Type, VectorType};
use kira_source::Span;

use super::{Checker, body::describe};
use crate::builtins;
use crate::diagnostics;
use crate::model::{
    BinaryOp, BuiltinFn, CheckedExpr, CheckedExprId, CheckedExprKind, ConstValue, UnaryOp,
};

impl Checker<'_> {
    /// Checks one expression.
    pub(crate) fn expr(&mut self, id: ExprId) -> CheckedExprId {
        let node = self.tree().expr(id).clone();
        match node {
            Expr::Int { value, .. } => {
                self.alloc(Type::Scalar(ScalarType::Int), literal_int(value))
            }
            Expr::Float { value, .. } => self.alloc(
                Type::Scalar(ScalarType::Float),
                CheckedExprKind::Const(ConstValue::Float(value)),
            ),
            Expr::Bool { value, .. } => self.alloc(
                Type::Scalar(ScalarType::Bool),
                CheckedExprKind::Const(ConstValue::Bool(value)),
            ),
            Expr::Name { symbol, span } => {
                let name = self.name(symbol);
                self.name_expr(&name, span)
            }
            Expr::Field { base, field, span } => {
                // `Ink.Low` reads as a member of a value named `Ink` right up
                // until nothing is named `Ink` — an enum is a namespace, not a
                // value, so its variants are found by the whole written path
                // before the base is checked and reported unbound.
                if let Some(id) = self.constant_path(id) {
                    return id;
                }
                let base = self.expr(base);
                let field = self.name(field);
                self.member(base, &field, span)
            }
            Expr::Index { base, index, span } => {
                let base = self.expr(base);
                let index = self.expr(index);
                let index = self.coerce(index, &Type::Scalar(ScalarType::Uint), span);
                match self.module.expr(base).ty.clone() {
                    Type::RuntimeArray(element) => {
                        self.alloc(*element, CheckedExprKind::Index { base, index })
                    }
                    Type::Vector(vector) => self.alloc(
                        Type::Scalar(vector.scalar),
                        CheckedExprKind::Index { base, index },
                    ),
                    other => {
                        self.reporter.error(
                            span,
                            diagnostics::TYPE_MISMATCH,
                            format!("`{}` cannot be indexed", describe(&other)),
                        );
                        self.invalid()
                    }
                }
            }
            Expr::Call { callee, args, span } => self.call(callee, &args, span),
            Expr::Unary { op, operand, span } => {
                let operand = self.expr(operand);
                self.unary(op, operand, span)
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let lhs = self.expr(lhs);
                let rhs = self.expr(rhs);
                self.binary(op, lhs, rhs, span)
            }
        }
    }

    /// Allocates a checked expression.
    pub(crate) fn alloc(&mut self, ty: Type, kind: CheckedExprKind) -> CheckedExprId {
        self.module.exprs.alloc(CheckedExpr { ty, kind })
    }

    /// The placeholder a rejected expression becomes.
    pub(crate) fn invalid(&mut self) -> CheckedExprId {
        self.alloc(Type::Void, CheckedExprKind::Invalid)
    }

    /// Resolves a bare name against locals, resources, and options.
    fn name_expr(&mut self, name: &str, span: Span) -> CheckedExprId {
        if let Some(ty) = self.lookup(name) {
            return self.alloc(ty, CheckedExprKind::Local(name.to_owned()));
        }
        if let Some(binding) = self.resources.get(name).cloned() {
            return self.alloc(binding.ty, CheckedExprKind::Resource(name.to_owned()));
        }
        if let Some((ty, _)) = self.options.get(name).cloned() {
            return self.alloc(ty, CheckedExprKind::Option(name.to_owned()));
        }
        // Inside an imported module, an unqualified name reaches that module's
        // own constant first — the same rule an unqualified *call* follows, and
        // for the same reason: a `const` is keyed with the importing alias
        // folded in, so its own file's bare reference has to look there too.
        let own = self.qualified(name);
        if let Some((ty, value)) = self
            .constants
            .get(&own)
            .or_else(|| self.constants.get(name))
            .cloned()
        {
            return self.alloc(ty, CheckedExprKind::Const(value));
        }
        self.reporter.error(
            span,
            diagnostics::UNKNOWN_NAME,
            format!("`{name}` is not bound here"),
        );
        self.invalid()
    }

    /// Reads a member: a struct field, or a vector component selection.
    fn member(&mut self, base: CheckedExprId, field: &str, span: Span) -> CheckedExprId {
        match self.module.expr(base).ty.clone() {
            Type::StructRef(name) => {
                let Some(found) = self
                    .structs
                    .get(&name)
                    .and_then(|fields| fields.iter().find(|declared| declared.name == field))
                    .cloned()
                else {
                    self.reporter.error(
                        span,
                        diagnostics::NO_SUCH_MEMBER,
                        format!("`{name}` has no member `{field}`"),
                    );
                    return self.invalid();
                };
                self.alloc(
                    found.ty,
                    CheckedExprKind::Field {
                        base,
                        field: field.to_owned(),
                    },
                )
            }
            Type::Vector(vector) => {
                let Some(components) = builtins::swizzle(field) else {
                    self.reporter.error(
                        span,
                        diagnostics::NO_SUCH_MEMBER,
                        format!("`{field}` is not a component selection"),
                    );
                    return self.invalid();
                };
                if let Some(&out_of_range) = components.iter().find(|&&index| index >= vector.width)
                {
                    self.reporter.error(
                        span,
                        diagnostics::NO_SUCH_MEMBER,
                        format!(
                            "component {} does not exist on a {}-wide vector",
                            "xyzw".chars().nth(out_of_range as usize).unwrap_or('?'),
                            vector.width
                        ),
                    );
                    return self.invalid();
                }
                let ty = if components.len() == 1 {
                    Type::Scalar(vector.scalar)
                } else {
                    Type::Vector(VectorType {
                        scalar: vector.scalar,
                        width: u8::try_from(components.len()).unwrap_or(4),
                    })
                };
                self.alloc(ty, CheckedExprKind::Swizzle { base, components })
            }
            // An array's one member is how many elements it holds. The binding
            // decides that at draw time, so it is never a constant here.
            Type::RuntimeArray(_) if field == "count" => self.alloc(
                Type::Scalar(ScalarType::Uint),
                CheckedExprKind::ArrayLength { base },
            ),
            other => {
                self.reporter.error(
                    span,
                    diagnostics::NO_SUCH_MEMBER,
                    format!("`{}` has no member `{field}`", describe(&other)),
                );
                self.invalid()
            }
        }
    }

    /// A call: a construction, a cast, a declared function, or a builtin.
    fn call(&mut self, callee: ExprId, args: &[ExprId], span: Span) -> CheckedExprId {
        let Some(written) = self.callee_name(callee) else {
            self.reporter.error(
                span,
                diagnostics::UNKNOWN_NAME,
                "only a name can be called in KSL",
            );
            return self.invalid();
        };
        // Inside an imported module, an unqualified call names that module's
        // own function first — `lambert` calling `saturate` in the same file
        // must reach `Lighting_saturate`, not look for a bare `saturate`.
        let own = self.qualified(&written);
        let name = if own != written && self.signatures.contains_key(&own) {
            own
        } else {
            written
        };
        if let Some(ty) = builtins::builtin_type(&name) {
            return self.construct(ty, args, span);
        }
        if self.structs.contains_key(&name) {
            return self.construct(Type::StructRef(name), args, span);
        }
        if let Some(which) = builtins::builtin_fn(&name) {
            return self.builtin_call(which, &name, args, span);
        }
        if let Some(signature) = self.signatures.get(&name).cloned() {
            if args.len() != signature.params.len() {
                self.reporter.error(
                    span,
                    diagnostics::ARGUMENT_COUNT,
                    format!(
                        "`{name}` takes {} argument(s), but {} were given",
                        signature.params.len(),
                        args.len()
                    ),
                );
                return self.invalid();
            }
            let checked = args
                .iter()
                .zip(&signature.params)
                .map(|(&arg, expected)| {
                    let arg = self.expr(arg);
                    self.coerce(arg, expected, span)
                })
                .collect();
            return self.alloc(
                signature.result,
                CheckedExprKind::Call {
                    name,
                    args: checked,
                },
            );
        }
        self.reporter.error(
            span,
            diagnostics::UNKNOWN_NAME,
            format!("`{name}` names no function or type"),
        );
        self.invalid()
    }

    /// The name a callee spells, when it spells one.
    ///
    /// A dotted callee is an imported function, whose name was flattened with
    /// the same separator when it was declared.
    fn callee_name(&mut self, callee: ExprId) -> Option<String> {
        match self.tree().expr(callee).clone() {
            Expr::Name { symbol, .. } => Some(self.name(symbol)),
            Expr::Field { base, field, .. } => {
                let Expr::Name { symbol, .. } = self.tree().expr(base).clone() else {
                    return None;
                };
                Some(format!("{}_{}", self.name(symbol), self.name(field)))
            }
            _ => None,
        }
    }

    /// Builds a value from its parts, or converts one scalar to another.
    fn construct(&mut self, ty: Type, args: &[ExprId], span: Span) -> CheckedExprId {
        let checked: Vec<CheckedExprId> = args.iter().map(|&arg| self.expr(arg)).collect();
        match &ty {
            // `UInt(x)` and `Float(x)` convert; every other scalar call is a
            // construction of one component, which is the same thing.
            Type::Scalar(_) if checked.len() == 1 => {
                let value = checked[0];
                let from = self.module.expr(value).ty.clone();
                if !is_numeric(&from) {
                    self.reporter.error(
                        span,
                        diagnostics::TYPE_MISMATCH,
                        format!("`{}` cannot be converted to a number", describe(&from)),
                    );
                    return self.invalid();
                }
                self.alloc(ty, CheckedExprKind::Cast { value })
            }
            Type::Vector(vector) => {
                let mut components = 0usize;
                for &arg in &checked {
                    components += match self.module.expr(arg).ty {
                        Type::Scalar(_) => 1,
                        Type::Vector(inner) => usize::from(inner.width),
                        _ => 0,
                    };
                }
                // One argument fills every lane, which is how the corpus writes
                // `Float4(0.0)`; otherwise the parts must add up exactly.
                let broadcast = checked.len() == 1 && components == 1;
                if !broadcast && components != usize::from(vector.width) {
                    self.reporter.error(
                        span,
                        diagnostics::ARGUMENT_COUNT,
                        format!(
                            "`{}` needs {} components, but {components} were given",
                            describe(&ty),
                            vector.width
                        ),
                    );
                    return self.invalid();
                }
                let element = Type::Scalar(vector.scalar);
                let coerced = checked
                    .into_iter()
                    .map(|arg| {
                        let target = match self.module.expr(arg).ty {
                            Type::Vector(inner) => Type::Vector(VectorType {
                                scalar: vector.scalar,
                                width: inner.width,
                            }),
                            _ => element.clone(),
                        };
                        self.coerce(arg, &target, span)
                    })
                    .collect();
                self.alloc(ty, CheckedExprKind::Construct { args: coerced })
            }
            Type::StructRef(name) => {
                let fields = self.structs.get(name).cloned().unwrap_or_default();
                if checked.len() != fields.len() {
                    self.reporter.error(
                        span,
                        diagnostics::ARGUMENT_COUNT,
                        format!(
                            "`{name}` has {} field(s), but {} were given",
                            fields.len(),
                            checked.len()
                        ),
                    );
                    return self.invalid();
                }
                let coerced = checked
                    .into_iter()
                    .zip(&fields)
                    .map(|(arg, field)| self.coerce(arg, &field.ty, span))
                    .collect();
                self.alloc(ty, CheckedExprKind::Construct { args: coerced })
            }
            Type::Matrix(_) => self.alloc(ty, CheckedExprKind::Construct { args: checked }),
            other => {
                self.reporter.error(
                    span,
                    diagnostics::TYPE_MISMATCH,
                    format!("`{}` cannot be constructed", describe(other)),
                );
                self.invalid()
            }
        }
    }

    /// A builtin call, checked for arity and given its result type.
    fn builtin_call(
        &mut self,
        which: BuiltinFn,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> CheckedExprId {
        let wanted = builtins::arity(which);
        if args.len() != wanted {
            self.reporter.error(
                span,
                diagnostics::ARGUMENT_COUNT,
                format!(
                    "`{name}` takes {wanted} argument(s), but {} were given",
                    args.len()
                ),
            );
            return self.invalid();
        }
        let checked: Vec<CheckedExprId> = args.iter().map(|&arg| self.expr(arg)).collect();
        let types: Vec<Type> = checked
            .iter()
            .map(|&id| self.module.expr(id).ty.clone())
            .collect();
        let float = Type::Scalar(ScalarType::Float);
        // The component-wise math family takes float scalars and float
        // vectors, with every vector argument sharing one shape — a scalar
        // beside a vector splats, which is what `mix(color, color, blend)`
        // means in every dialect. The vector family (`dot`, `cross`,
        // `normalize`, `length`) takes only same-shaped float vectors.
        // Checking here is what keeps `cross(1.0, 2.0)` from compiling
        // "clean" and reaching backends as a call they cannot spell.
        if which.wants_floats() || which.wants_vectors() {
            let mut vector_shape: Option<&Type> = None;
            let mut shaped = true;
            for ty in &types {
                match ty {
                    Type::Scalar(ScalarType::Float) => {}
                    Type::Vector(_) if vector_shape.is_none_or(|shape| shape == ty) => {
                        vector_shape = Some(ty)
                    }
                    _ => shaped = false,
                }
            }
            // The vector family refuses scalar arguments outright, and
            // `cross` is the one of them whose width every dialect pins.
            if which.wants_vectors() && vector_shape.is_none() {
                shaped = false;
            }
            if which == BuiltinFn::Cross
                && vector_shape.is_some_and(|shape| {
                    shape
                        != &Type::Vector(VectorType {
                            scalar: ScalarType::Float,
                            width: 3,
                        })
                })
            {
                shaped = false;
            }
            if !shaped {
                self.reporter.error(
                    span,
                    diagnostics::TYPE_MISMATCH,
                    format!(
                        "`{name}` takes {}of one shape; got {}",
                        if which.wants_vectors() {
                            "float vectors "
                        } else {
                            "floats or float vectors "
                        },
                        types.iter().map(describe).collect::<Vec<_>>().join(", ")
                    ),
                );
                return self.invalid();
            }
        }
        let result = match which {
            // `mul` is the one call whose operands each dialect orders its own
            // way, so its result follows the second operand's shape.
            BuiltinFn::Mul => match (&types[0], &types[1]) {
                (Type::Matrix(matrix), Type::Vector(_)) => Type::Vector(VectorType {
                    scalar: ScalarType::Float,
                    width: matrix.rows,
                }),
                (Type::Matrix(_), Type::Matrix(right)) => Type::Matrix(*right),
                _ => {
                    self.reporter.error(
                        span,
                        diagnostics::TYPE_MISMATCH,
                        "`mul` multiplies a matrix by a vector or another matrix",
                    );
                    return self.invalid();
                }
            },
            BuiltinFn::Dot | BuiltinFn::Length => float,
            BuiltinFn::Sample => {
                let uv = Type::Vector(VectorType {
                    scalar: ScalarType::Float,
                    width: 2,
                });
                let coerced = self.coerce(checked[2], &uv, span);
                return self.alloc(
                    Type::Vector(VectorType {
                        scalar: ScalarType::Float,
                        width: 4,
                    }),
                    CheckedExprKind::Builtin {
                        which,
                        args: vec![checked[0], checked[1], coerced],
                    },
                );
            }
            BuiltinFn::Load => {
                let texel = match &types[0] {
                    Type::Texture(kira_shader_model::TextureDimension::Texture2dUint) => {
                        ScalarType::Uint
                    }
                    _ => ScalarType::Float,
                };
                Type::Vector(VectorType {
                    scalar: texel,
                    width: 4,
                })
            }
            // Called for its effect: a store writes a texel and yields nothing,
            // so using its result is a type error rather than a silent zero.
            BuiltinFn::Store => Type::Void,
            BuiltinFn::AtomicAdd => Type::Scalar(ScalarType::Uint),
            // Everything else is component-wise: the result is the first
            // operand's shape.
            _ => types[0].clone(),
        };
        self.alloc(
            result,
            CheckedExprKind::Builtin {
                which,
                args: checked,
            },
        )
    }

    /// A prefix operator.
    fn unary(&mut self, op: SyntaxUnaryOp, operand: CheckedExprId, span: Span) -> CheckedExprId {
        let ty = self.module.expr(operand).ty.clone();
        match op {
            SyntaxUnaryOp::Neg if is_numeric(&ty) => self.alloc(
                ty,
                CheckedExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand,
                },
            ),
            SyntaxUnaryOp::Not if ty == Type::Scalar(ScalarType::Bool) => self.alloc(
                ty,
                CheckedExprKind::Unary {
                    op: UnaryOp::Not,
                    operand,
                },
            ),
            _ => {
                self.reporter.error(
                    span,
                    diagnostics::BAD_OPERATOR,
                    format!("`{}` does not apply to `{}`", op.spelling(), describe(&ty)),
                );
                self.invalid()
            }
        }
    }

    /// An infix operator, with the scalar-broadcast rule shaders expect.
    fn binary(
        &mut self,
        op: SyntaxBinaryOp,
        lhs: CheckedExprId,
        rhs: CheckedExprId,
        span: Span,
    ) -> CheckedExprId {
        let op = translate(op);
        let left = self.module.expr(lhs).ty.clone();
        let right = self.module.expr(rhs).ty.clone();
        if left == Type::Void || right == Type::Void {
            // One side already reported; saying so again would be noise.
            return self.invalid();
        }

        // A literal on either side takes the other side's type, which is what
        // makes `count - 1` and `0.5 * value` both work.
        let (lhs, rhs, left, right) = if left != right {
            if self.is_literal(rhs) {
                let rhs = self.coerce(rhs, &element_of(&left), span);
                let right = self.module.expr(rhs).ty.clone();
                (lhs, rhs, left, right)
            } else if self.is_literal(lhs) {
                let lhs = self.coerce(lhs, &element_of(&right), span);
                let left = self.module.expr(lhs).ty.clone();
                (lhs, rhs, left, right)
            } else {
                (lhs, rhs, left, right)
            }
        } else {
            (lhs, rhs, left, right)
        };

        // An ordering over booleans has no answer in any dialect; equality
        // does, so only the orderings are refused here.
        if op.is_ordering() && left == Type::Scalar(ScalarType::Bool) {
            self.reporter.error(
                span,
                diagnostics::BAD_OPERATOR,
                format!(
                    "`{}` orders values; booleans combine with `&&`, `||`, `!`, and `==`",
                    op_spelling(op)
                ),
            );
            return self.invalid();
        }
        let result = if op.is_logical() {
            // A logical connective takes two booleans, period: `&&`/`||` on
            // numbers or vectors would compile to different operators in
            // different dialects (C's truthiness versus WGSL's type error),
            // which is exactly the disagreement KSL exists to prevent.
            let boolean = Type::Scalar(ScalarType::Bool);
            match (&left, &right) {
                (Type::Scalar(ScalarType::Bool), Type::Scalar(ScalarType::Bool)) => {
                    let lhs = self.coerce(lhs, &boolean, span);
                    let rhs = self.coerce(rhs, &boolean, span);
                    return self.alloc(boolean, CheckedExprKind::Binary { op, lhs, rhs });
                }
                _ => {
                    self.reporter.error(
                        span,
                        diagnostics::BAD_OPERATOR,
                        format!(
                            "`{}` combines two booleans, not `{}` and `{}`",
                            op_spelling(op),
                            describe(&left),
                            describe(&right)
                        ),
                    );
                    return self.invalid();
                }
            }
        } else if op.is_comparison() {
            match (&left, &right) {
                _ if left == right => Type::Scalar(ScalarType::Bool),
                _ => {
                    self.reporter.error(
                        span,
                        diagnostics::BAD_OPERATOR,
                        format!(
                            "`{}` compares two values of one type, not `{}` and `{}`",
                            op_spelling(op),
                            describe(&left),
                            describe(&right)
                        ),
                    );
                    return self.invalid();
                }
            }
        } else {
            match (&left, &right) {
                _ if left == right => left.clone(),
                // Scaling a vector by a scalar, in either order.
                (Type::Vector(_), Type::Scalar(scalar))
                    if vector_scalar(&left) == Some(*scalar) =>
                {
                    left.clone()
                }
                (Type::Scalar(scalar), Type::Vector(_))
                    if vector_scalar(&right) == Some(*scalar) =>
                {
                    right.clone()
                }
                _ => {
                    self.reporter.error(
                        span,
                        diagnostics::BAD_OPERATOR,
                        format!(
                            "`{}` does not apply to `{}` and `{}`",
                            op_spelling(op),
                            describe(&left),
                            describe(&right)
                        ),
                    );
                    return self.invalid();
                }
            }
        };
        self.alloc(result, CheckedExprKind::Binary { op, lhs, rhs })
    }

    /// Whether `id` is a literal, and so free to retype.
    fn is_literal(&self, id: CheckedExprId) -> bool {
        matches!(self.module.expr(id).kind, CheckedExprKind::Const(_))
    }

    /// Retypes `id` to `expected` when it may, reporting when it may not.
    pub(crate) fn coerce(
        &mut self,
        id: CheckedExprId,
        expected: &Type,
        span: Span,
    ) -> CheckedExprId {
        let actual = self.module.expr(id).ty.clone();
        if actual == *expected || actual == Type::Void || *expected == Type::Void {
            return id;
        }
        if let (CheckedExprKind::Const(value), Type::Scalar(scalar)) =
            (self.module.expr(id).kind.clone(), expected)
            && let Some(retyped) = retype(value, *scalar)
        {
            return self.alloc(expected.clone(), CheckedExprKind::Const(retyped));
        }
        self.reporter.error(
            span,
            diagnostics::TYPE_MISMATCH,
            format!(
                "expected `{}`, found `{}`",
                describe(expected),
                describe(&actual)
            ),
        );
        id
    }

    /// The dotted path `id` writes, flattened the way an emitted name is.
    ///
    /// `Ink.Low` becomes `Ink_Low` and `Shared.Ink.Low` becomes
    /// `Shared_Ink_Low`, which is what an imported declaration is keyed as.
    /// Anything that is not a chain of names has no path.
    fn written_path(&self, id: ExprId) -> Option<String> {
        match self.tree().expr(id).clone() {
            Expr::Name { symbol, .. } => Some(self.name(symbol)),
            Expr::Field { base, field, .. } => {
                Some(format!("{}_{}", self.written_path(base)?, self.name(field)))
            }
            _ => None,
        }
    }

    /// The constant `id`'s written path names, already checked.
    fn constant_path(&mut self, id: ExprId) -> Option<CheckedExprId> {
        let path = self.written_path(id)?;
        let (ty, value) = self.constants.get(&path).cloned()?;
        Some(self.alloc(ty, CheckedExprKind::Const(value)))
    }

    /// Folds `id` to a constant of `expected`, when it is one.
    pub(crate) fn constant(&mut self, id: ExprId, expected: &Type) -> Option<ConstValue> {
        let node = self.tree().expr(id).clone();
        let Type::Scalar(scalar) = expected else {
            return None;
        };
        match node {
            Expr::Int { value, .. } => retype(literal_value(value), *scalar),
            Expr::Float { value, .. } => retype(ConstValue::Float(value), *scalar),
            Expr::Bool { value, .. } => retype(ConstValue::Bool(value), *scalar),
            Expr::Name { symbol, .. } => {
                let name = self.name(symbol);
                self.options
                    .get(&name)
                    .or_else(|| self.constants.get(&self.qualified(&name)))
                    .or_else(|| self.constants.get(&name))
                    .map(|(_, value)| *value)
                    .and_then(|value| retype(value, *scalar))
            }
            Expr::Field { .. } => {
                let path = self.written_path(id)?;
                let (_, value) = self.constants.get(&path)?;
                retype(*value, *scalar)
            }
            _ => None,
        }
    }
}

/// The kind an integer literal starts as.
fn literal_int(value: u64) -> CheckedExprKind {
    CheckedExprKind::Const(literal_value(value))
}

/// The constant an integer literal starts as.
fn literal_value(value: u64) -> ConstValue {
    i64::try_from(value).map_or(ConstValue::Uint(value), ConstValue::Int)
}

/// `value` as `scalar`, when the conversion loses nothing that matters.
fn retype(value: ConstValue, scalar: ScalarType) -> Option<ConstValue> {
    Some(match (value, scalar) {
        (ConstValue::Bool(value), ScalarType::Bool) => ConstValue::Bool(value),
        (ConstValue::Int(value), ScalarType::Int) => ConstValue::Int(value),
        (ConstValue::Int(value), ScalarType::Uint) => ConstValue::Uint(u64::try_from(value).ok()?),
        #[allow(clippy::cast_precision_loss)]
        (ConstValue::Int(value), ScalarType::Float) => ConstValue::Float(value as f64),
        (ConstValue::Uint(value), ScalarType::Uint) => ConstValue::Uint(value),
        (ConstValue::Uint(value), ScalarType::Int) => ConstValue::Int(i64::try_from(value).ok()?),
        #[allow(clippy::cast_precision_loss)]
        (ConstValue::Uint(value), ScalarType::Float) => ConstValue::Float(value as f64),
        (ConstValue::Float(value), ScalarType::Float) => ConstValue::Float(value),
        _ => return None,
    })
}

/// Whether arithmetic applies to `ty`.
fn is_numeric(ty: &Type) -> bool {
    match ty {
        Type::Scalar(scalar) => *scalar != ScalarType::Bool,
        Type::Vector(vector) => vector.scalar != ScalarType::Bool,
        Type::Matrix(_) => true,
        _ => false,
    }
}

/// The scalar a vector holds, when `ty` is one.
fn vector_scalar(ty: &Type) -> Option<ScalarType> {
    match ty {
        Type::Vector(vector) => Some(vector.scalar),
        _ => None,
    }
}

/// The scalar type a literal beside `ty` should take.
fn element_of(ty: &Type) -> Type {
    match ty {
        Type::Vector(vector) => Type::Scalar(vector.scalar),
        Type::Matrix(_) => Type::Scalar(ScalarType::Float),
        other => other.clone(),
    }
}

/// The checked operator a written one becomes.
fn translate(op: SyntaxBinaryOp) -> BinaryOp {
    match op {
        SyntaxBinaryOp::Add => BinaryOp::Add,
        SyntaxBinaryOp::Sub => BinaryOp::Sub,
        SyntaxBinaryOp::Mul => BinaryOp::Mul,
        SyntaxBinaryOp::Div => BinaryOp::Div,
        SyntaxBinaryOp::Rem => BinaryOp::Rem,
        SyntaxBinaryOp::Eq => BinaryOp::Eq,
        SyntaxBinaryOp::Ne => BinaryOp::Ne,
        SyntaxBinaryOp::Lt => BinaryOp::Lt,
        SyntaxBinaryOp::Le => BinaryOp::Le,
        SyntaxBinaryOp::Gt => BinaryOp::Gt,
        SyntaxBinaryOp::Ge => BinaryOp::Ge,
        SyntaxBinaryOp::And => BinaryOp::And,
        SyntaxBinaryOp::Or => BinaryOp::Or,
        SyntaxBinaryOp::BitAnd => BinaryOp::BitAnd,
        SyntaxBinaryOp::BitOr => BinaryOp::BitOr,
        SyntaxBinaryOp::BitXor => BinaryOp::BitXor,
        SyntaxBinaryOp::Shl => BinaryOp::Shl,
        SyntaxBinaryOp::Shr => BinaryOp::Shr,
    }
}

/// How a checked operator is written.
fn op_spelling(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "^",
        BinaryOp::Shl => "<<",
        BinaryOp::Shr => ">>",
    }
}
