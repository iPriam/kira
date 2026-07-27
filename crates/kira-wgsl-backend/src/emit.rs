//! Emitting WGSL: types, expressions, statements, and declarations.

use kira_ksl_semantics::model::{
    BinaryOp, BuiltinFn, CheckedExprId, CheckedExprKind, CheckedFunction, CheckedModule,
    CheckedStmt, CheckedStmtId, ConstValue, UnaryOp,
};
use kira_shader_model::{Builtin, Reflection, ScalarType, Stage, TextureDimension, Type};

/// The WGSL spelling of a type.
#[must_use]
pub fn type_name(ty: &Type) -> String {
    let scalar = |scalar| match scalar {
        ScalarType::Bool => "bool",
        ScalarType::Int => "i32",
        ScalarType::Uint => "u32",
        ScalarType::Float => "f32",
    };
    match ty {
        Type::Void => "void".to_owned(),
        Type::Scalar(value) => scalar(*value).to_owned(),
        Type::Vector(vector) => format!("vec{}<{}>", vector.width, scalar(vector.scalar)),
        Type::Matrix(matrix) => format!("mat{}x{}<f32>", matrix.columns, matrix.rows),
        Type::StructRef(name) => name.clone(),
        Type::Texture(dimension) => match dimension {
            TextureDimension::Texture2d => "texture_2d<f32>".to_owned(),
            TextureDimension::TextureCube => "texture_cube<f32>".to_owned(),
            TextureDimension::Depth2d => "texture_depth_2d".to_owned(),
            TextureDimension::Texture2dUint => "texture_2d<u32>".to_owned(),
        },
        Type::Sampler(kind) => match kind {
            kira_shader_model::SamplerKind::Filtering => "sampler".to_owned(),
            kira_shader_model::SamplerKind::Comparison => "sampler_comparison".to_owned(),
        },
        Type::RuntimeArray(element) => format!("array<{}>", type_name(element)),
    }
}

/// The attribute a builtin carries on an interface field.
#[must_use]
pub fn builtin_attribute(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Position | Builtin::FragCoord => "@builtin(position)",
        Builtin::VertexIndex => "@builtin(vertex_index)",
        Builtin::InstanceIndex => "@builtin(instance_index)",
        Builtin::FrontFacing => "@builtin(front_facing)",
        Builtin::ThreadId => "@builtin(global_invocation_id)",
        Builtin::LocalId => "@builtin(local_invocation_id)",
        Builtin::GroupId => "@builtin(workgroup_id)",
        Builtin::LocalIndex => "@builtin(local_invocation_index)",
    }
}

/// The running emission.
pub(crate) struct Emitter<'a> {
    pub(crate) module: &'a CheckedModule,
    pub(crate) reflection: &'a Reflection,
    pub(crate) out: String,
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
            .map(|param| format!("{}: {}", param.name, type_name(&param.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let signature = if function.result == Type::Void {
            format!("fn {}({params}) {{", function.name)
        } else {
            format!(
                "fn {}({params}) -> {} {{",
                function.name,
                type_name(&function.result)
            )
        };
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
    fn stmt(&mut self, id: CheckedStmtId, depth: usize) {
        match self.module.stmt(id).clone() {
            // WGSL's `let` is immutable and KSL's is not — the corpus reassigns
            // bindings constantly — so every local becomes a `var`.
            CheckedStmt::Let { name, ty, init } => {
                let declared = match init {
                    None => format!("var {name}: {} = {}();", type_name(&ty), type_name(&ty)),
                    Some(value) => {
                        format!("var {name}: {} = {};", type_name(&ty), self.expr(value))
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
            // WGSL asks the binding itself, so nothing has to be passed in.
            CheckedExprKind::ArrayLength { base } => {
                format!("arrayLength(&{})", self.expr(*base))
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

    /// Renders a builtin call in WGSL's spelling.
    fn builtin(&self, which: BuiltinFn, args: &[CheckedExprId]) -> String {
        let at = |index: usize| {
            args.get(index)
                .map_or_else(String::new, |&id| self.expr(id))
        };
        match which {
            BuiltinFn::Mul => format!("({} * {})", at(0), at(1)),
            BuiltinFn::Sample => format!("textureSample({}, {}, {})", at(0), at(1), at(2)),
            // `textureLoad` needs integer texel coordinates and an explicit
            // mip level; KSL's `load` names neither, so level 0 is implied.
            BuiltinFn::Load => format!("textureLoad({}, vec2<i32>({}), 0)", at(0), at(1)),
            BuiltinFn::AtomicAdd => format!("atomicAdd(&{}[{}], {})", at(0), at(1), at(2)),
            BuiltinFn::Abs => format!("abs({})", at(0)),
            BuiltinFn::Atan2 => format!("atan2({}, {})", at(0), at(1)),
            BuiltinFn::Fract => format!("fract({})", at(0)),
            BuiltinFn::Mix => format!("mix({}, {}, {})", at(0), at(1), at(2)),
            other => {
                let name = match other {
                    BuiltinFn::Dot => "dot",
                    BuiltinFn::Cross => "cross",
                    BuiltinFn::Normalize => "normalize",
                    BuiltinFn::Length => "length",
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

    /// Writes every resource as a module-scope binding.
    ///
    /// WGSL declares resources at module scope rather than as entry-point
    /// parameters, so one declaration serves every stage that reads it.
    pub(crate) fn resources(&mut self) {
        let resources = self.reflection.resources.clone();
        for resource in &resources {
            let Some(binding) = resource
                .backend_bindings
                .iter()
                .find(|binding| binding.target == kira_shader_model::BackendTarget::Wgsl)
            else {
                continue;
            };
            let at = format!(
                "@group({}) @binding({})",
                binding.group_index, binding.binding_index
            );
            let name = &resource.resource_name;
            let declared = match resource.resource_kind {
                kira_shader_model::ResourceKind::Uniform => {
                    format!("{at} var<uniform> {name}: {};", resource.type_name)
                }
                kira_shader_model::ResourceKind::Storage => {
                    let access =
                        if resource.access == Some(kira_shader_model::AccessMode::ReadWrite) {
                            "read_write"
                        } else {
                            "read"
                        };
                    format!(
                        "{at} var<storage, {access}> {name}: array<{}>;",
                        element_name(&resource.type_name)
                    )
                }
                kira_shader_model::ResourceKind::Texture => {
                    format!("{at} var {name}: {};", texture_name(&resource.type_name))
                }
                kira_shader_model::ResourceKind::Sampler => {
                    format!("{at} var {name}: sampler;")
                }
            };
            self.line(0, &declared);
        }
        if !resources.is_empty() {
            self.out.push('\n');
        }
    }
}

/// The element type inside a reflected `[T]`.
fn element_name(type_name: &str) -> String {
    type_name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map_or_else(|| type_name.to_owned(), wgsl_name)
}

/// The WGSL spelling of a KSL type written as a name.
pub(crate) fn wgsl_name(name: &str) -> String {
    kira_ksl_semantics::builtins::builtin_type(name)
        .map_or_else(|| name.to_owned(), |ty| type_name(&ty))
}

/// The WGSL texture type a reflected texture name spells.
fn texture_name(name: &str) -> &'static str {
    match name {
        "Texture2dUint" => "texture_2d<u32>",
        "TextureCube" => "texture_cube<f32>",
        "Depth2d" => "texture_depth_2d",
        _ => "texture_2d<f32>",
    }
}

/// A constant, written so its type survives.
///
/// WGSL has no implicit numeric conversion, so an untagged `1` beside a `u32`
/// is an error rather than a promotion — every literal carries its suffix.
fn constant(value: ConstValue) -> String {
    match value {
        ConstValue::Bool(value) => value.to_string(),
        ConstValue::Int(value) => format!("{value}i"),
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

/// How a binary operator is written in WGSL.
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

/// The attribute opening a stage's entry point.
#[must_use]
pub fn stage_attribute(stage: Stage, threads: Option<[u32; 3]>) -> String {
    match stage {
        Stage::Vertex => "@vertex".to_owned(),
        Stage::Fragment => "@fragment".to_owned(),
        Stage::Compute => {
            let [x, y, z] = threads.unwrap_or([1, 1, 1]);
            format!("@compute @workgroup_size({x}, {y}, {z})")
        }
    }
}
