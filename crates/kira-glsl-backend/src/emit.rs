//! Emitting GLSL 330: types, expressions, and statements.

use kira_ksl_semantics::model::{
    BinaryOp, BuiltinFn, CheckedExprId, CheckedExprKind, CheckedFunction, CheckedModule,
    CheckedStmt, CheckedStmtId, ConstValue, UnaryOp,
};
use kira_shader_model::{Builtin, Reflection, ScalarType, Stage, Type};

/// The GLSL spelling of a type.
#[must_use]
pub fn type_name(ty: &Type) -> String {
    let scalar = |scalar| match scalar {
        ScalarType::Bool => "bool",
        ScalarType::Int => "int",
        ScalarType::Uint => "uint",
        ScalarType::Float => "float",
    };
    let prefix = |scalar| match scalar {
        ScalarType::Bool => "b",
        ScalarType::Int => "i",
        ScalarType::Uint => "u",
        ScalarType::Float => "",
    };
    match ty {
        Type::Void => "void".to_owned(),
        Type::Scalar(value) => scalar(*value).to_owned(),
        Type::Vector(vector) => format!("{}vec{}", prefix(vector.scalar), vector.width),
        // `mat4` rather than `mat4x4`: both are legal, and the short spelling
        // is what a square matrix is written as everywhere in GLSL.
        Type::Matrix(matrix) if matrix.columns == matrix.rows => {
            format!("mat{}", matrix.columns)
        }
        Type::Matrix(matrix) => format!("mat{}x{}", matrix.columns, matrix.rows),
        Type::StructRef(name) => name.clone(),
        Type::Texture(dimension) => match dimension {
            kira_shader_model::TextureDimension::TextureCube => "samplerCube".to_owned(),
            kira_shader_model::TextureDimension::Depth2d => "sampler2DShadow".to_owned(),
            kira_shader_model::TextureDimension::Texture2dUint => "usampler2D".to_owned(),
            kira_shader_model::TextureDimension::Texture2d => "sampler2D".to_owned(),
        },
        // GLSL 330 has no standalone sampler object, so one never reaches a
        // declaration: it is folded into the texture it reads.
        Type::Sampler(_) => String::new(),
        Type::RuntimeArray(element) => format!("{}[]", type_name(element)),
    }
}

/// The GLSL name a builtin is read or written through in `stage`.
///
/// The stage is load-bearing for exactly one of them. `@builtin(position)` is
/// one KSL annotation and two GLSL names: `gl_Position` is the clip-space
/// position a vertex stage writes, and `gl_FragCoord` is the window-space
/// coordinate a fragment stage reads. A fragment shader naming `gl_Position`
/// does not compile — it is not declared there at all.
#[must_use]
pub fn builtin_name(builtin: Builtin, stage: Stage) -> &'static str {
    match builtin {
        Builtin::Position if stage == Stage::Fragment => "gl_FragCoord",
        Builtin::Position => "gl_Position",
        Builtin::FragCoord => "gl_FragCoord",
        Builtin::VertexIndex => "gl_VertexID",
        Builtin::InstanceIndex => "gl_InstanceID",
        Builtin::FrontFacing => "gl_FrontFacing",
        // GLSL 330 has no compute stage, so these never appear; the names are
        // 430's, and emission refuses a compute shader before reaching them.
        Builtin::ThreadId => "gl_GlobalInvocationID",
        Builtin::LocalId => "gl_LocalInvocationID",
        Builtin::GroupId => "gl_WorkGroupID",
        Builtin::LocalIndex => "gl_LocalInvocationIndex",
    }
}

/// The running emission.
pub(crate) struct Emitter<'a> {
    pub(crate) module: &'a CheckedModule,
    pub(crate) reflection: &'a Reflection,
    pub(crate) out: String,
    /// The sampler names folded into a texture, which never get a declaration.
    pub(crate) samplers: Vec<String>,
}

impl Emitter<'_> {
    /// Writes `text` indented by `depth` levels.
    pub(crate) fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// Writes a function, its parameters, and its body.
    pub(crate) fn function(&mut self, function: &CheckedFunction) {
        let params = function
            .params
            .iter()
            .map(|param| format!("{} {}", type_name(&param.ty), param.name))
            .collect::<Vec<_>>()
            .join(", ");
        let signature = format!(
            "{} {}({params}) {{",
            type_name(&function.result),
            function.name
        );
        self.line(0, &signature);
        self.body(&function.body, 1);
        self.line(0, "}");
        self.out.push('\n');
    }

    /// Writes a sequence of statements.
    pub(crate) fn body(&mut self, stmts: &[CheckedStmtId], depth: usize) {
        for &id in stmts {
            self.stmt(id, depth);
        }
    }

    /// Writes one statement.
    pub(crate) fn stmt(&mut self, id: CheckedStmtId, depth: usize) {
        match self.module.stmt(id).clone() {
            CheckedStmt::Let { name, ty, init } => {
                let declared = match init {
                    // GLSL has no aggregate default initializer, so an
                    // uninitialized declaration is what a body-filled value is.
                    None => format!("{} {name};", type_name(&ty)),
                    Some(value) => {
                        format!("{} {name} = {};", type_name(&ty), self.expr(value))
                    }
                };
                self.line(depth, &declared);
            }
            CheckedStmt::Assign { target, value } => {
                let assignment = format!("{} = {};", self.expr(target), self.expr(value));
                self.line(depth, &assignment);
            }
            CheckedStmt::If {
                cond,
                then,
                otherwise,
            } => {
                let opened = format!("if ({}) {{", self.expr(cond));
                self.line(depth, &opened);
                self.body(&then, depth + 1);
                match otherwise {
                    Some(otherwise) => {
                        self.line(depth, "} else {");
                        self.body(&otherwise, depth + 1);
                        self.line(depth, "}");
                    }
                    None => self.line(depth, "}"),
                }
            }
            CheckedStmt::While { cond, body } => {
                let opened = format!("while ({}) {{", self.expr(cond));
                self.line(depth, &opened);
                self.body(&body, depth + 1);
                self.line(depth, "}");
            }
            CheckedStmt::Return(None) => self.line(depth, "return;"),
            CheckedStmt::Return(Some(value)) => {
                let returned = format!("return {};", self.expr(value));
                self.line(depth, &returned);
            }
            CheckedStmt::Expr(value) => {
                let evaluated = format!("{};", self.expr(value));
                self.line(depth, &evaluated);
            }
        }
    }

    /// Renders one expression.
    pub(crate) fn expr(&self, id: CheckedExprId) -> String {
        let node = self.module.expr(id);
        match &node.kind {
            CheckedExprKind::Const(value) => constant(*value),
            CheckedExprKind::Local(name)
            | CheckedExprKind::Option(name)
            | CheckedExprKind::Resource(name) => name.clone(),
            CheckedExprKind::Field { base, field } => {
                format!("{}.{field}", self.expr(*base))
            }
            CheckedExprKind::Swizzle { base, components } => {
                let letters: String = components
                    .iter()
                    .map(|&at| "xyzw".chars().nth(at as usize).unwrap_or('x'))
                    .collect();
                format!("{}.{letters}", self.expr(*base))
            }
            CheckedExprKind::ArrayLength { base } => {
                format!("uint({}.length())", self.expr(*base))
            }
            CheckedExprKind::Index { base, index } => {
                format!("{}[{}]", self.expr(*base), self.expr(*index))
            }
            CheckedExprKind::Construct { args } => {
                format!("{}({})", type_name(&node.ty), self.args(args))
            }
            CheckedExprKind::Cast { value } => {
                format!("{}({})", type_name(&node.ty), self.expr(*value))
            }
            CheckedExprKind::Call { name, args } => {
                format!("{name}({})", self.args(args))
            }
            CheckedExprKind::Builtin { which, args } => self.builtin(*which, args),
            CheckedExprKind::Unary { op, operand } => {
                let spelling = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "!",
                };
                format!("({spelling}{})", self.expr(*operand))
            }
            CheckedExprKind::Binary { op, lhs, rhs } => {
                format!(
                    "({} {} {})",
                    self.expr(*lhs),
                    binary_spelling(*op),
                    self.expr(*rhs)
                )
            }
            CheckedExprKind::Invalid => "0".to_owned(),
        }
    }

    /// Renders a comma-separated argument list.
    fn args(&self, args: &[CheckedExprId]) -> String {
        args.iter()
            .map(|&arg| self.expr(arg))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Renders a builtin call in GLSL's spelling.
    fn builtin(&self, which: BuiltinFn, args: &[CheckedExprId]) -> String {
        let at = |index: usize| {
            args.get(index)
                .map_or_else(String::new, |&id| self.expr(id))
        };
        match which {
            BuiltinFn::Mul => format!("({} * {})", at(0), at(1)),
            // The sampler argument has no GLSL 330 counterpart: the texture
            // uniform already carries the sampling state, so it is dropped.
            BuiltinFn::Sample => format!("texture({}, {})", at(0), at(2)),
            BuiltinFn::Load => format!("texelFetch({}, ivec2({}), 0)", at(0), at(1)),
            BuiltinFn::AtomicAdd => format!("atomicAdd({}[{}], {})", at(0), at(1), at(2)),
            BuiltinFn::Fract => format!("fract({})", at(0)),
            BuiltinFn::Atan2 => format!("atan({}, {})", at(0), at(1)),
            BuiltinFn::Mix => format!("mix({}, {}, {})", at(0), at(1), at(2)),
            other => {
                let name = match other {
                    BuiltinFn::Dot => "dot",
                    BuiltinFn::Cross => "cross",
                    BuiltinFn::Normalize => "normalize",
                    BuiltinFn::Length => "length",
                    BuiltinFn::Abs => "abs",
                    BuiltinFn::Floor => "floor",
                    BuiltinFn::Ceil => "ceil",
                    BuiltinFn::Min => "min",
                    BuiltinFn::Max => "max",
                    BuiltinFn::Clamp => "clamp",
                    BuiltinFn::Step => "step",
                    BuiltinFn::Smoothstep => "smoothstep",
                    BuiltinFn::Pow => "pow",
                    BuiltinFn::Sqrt => "sqrt",
                    BuiltinFn::Sin => "sin",
                    BuiltinFn::Cos => "cos",
                    BuiltinFn::Tan => "tan",
                    BuiltinFn::Exp => "exp",
                    BuiltinFn::Log => "log",
                    _ => "",
                };
                format!("{name}({})", self.args(args))
            }
        }
    }

    /// Writes every resource as a uniform declaration.
    pub(crate) fn resources(&mut self) {
        let resources = self.reflection.resources.clone();
        let mut wrote = false;
        for resource in &resources {
            let name = &resource.resource_name;
            match resource.resource_kind {
                kira_shader_model::ResourceKind::Uniform => {
                    // A `std140` block, which is the layout every backend here
                    // agrees on, named so the body's `camera.field` still reads.
                    let opened = format!("layout(std140) uniform {}_block {{", resource.type_name);
                    self.line(0, &opened);
                    if let Some(declared) = self.module.struct_named(&resource.type_name) {
                        for field in &declared.fields.clone() {
                            let line = format!("{} {};", type_name(&field.ty), field.name);
                            self.line(1, &line);
                        }
                    }
                    let closed = format!("}} {name};");
                    self.line(0, &closed);
                    wrote = true;
                }
                kira_shader_model::ResourceKind::Texture => {
                    let line = format!("uniform {} {name};", texture_name(&resource.type_name));
                    self.line(0, &line);
                    wrote = true;
                }
                // A sampler folds into the texture it reads and never becomes
                // a declaration of its own.
                kira_shader_model::ResourceKind::Sampler => {
                    self.samplers.push(name.clone());
                }
                // Storage needs `430`; emission refuses such a shader before
                // it reaches here.
                kira_shader_model::ResourceKind::Storage => {}
            }
        }
        if wrote {
            self.out.push('\n');
        }
    }
}

/// The GLSL sampler type a reflected texture name spells.
fn texture_name(name: &str) -> &'static str {
    match name {
        "Texture2dUint" => "usampler2D",
        "TextureCube" => "samplerCube",
        "Depth2d" => "sampler2DShadow",
        _ => "sampler2D",
    }
}

/// The GLSL spelling of a KSL type written as a name.
pub(crate) fn glsl_name(name: &str) -> String {
    kira_ksl_semantics::builtins::builtin_type(name)
        .map_or_else(|| name.to_owned(), |ty| type_name(&ty))
}

/// A constant, written so its type survives.
fn constant(value: ConstValue) -> String {
    match value {
        ConstValue::Bool(value) => value.to_string(),
        ConstValue::Int(value) => value.to_string(),
        ConstValue::Uint(value) => format!("{value}u"),
        ConstValue::Float(value) => {
            if value.fract() == 0.0 && value.is_finite() {
                format!("{value:.1}")
            } else {
                format!("{value:?}")
            }
        }
    }
}

/// How a binary operator is written in GLSL.
fn binary_spelling(op: BinaryOp) -> &'static str {
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
