//! Emitting MSL: types, expressions, statements, and entry-point signatures.

use kira_ksl_semantics::model::{
    BinaryOp, BuiltinFn, CheckedExprId, CheckedExprKind, CheckedFunction, CheckedModule,
    CheckedStmt, CheckedStmtId, ConstValue, UnaryOp,
};
use kira_shader_model::{
    Builtin, ReflectedResource, Reflection, ResourceKind, ScalarType, Stage, TextureDimension, Type,
};

/// The first Metal buffer index an array-length buffer may take.
///
/// Bind-group slots run from the bottom of the table; counts sit above them so
/// the two can never collide.
const COUNT_BUFFER_BASE: u32 = 16;

/// The MSL spelling of a type.
#[must_use]
pub fn type_name(ty: &Type) -> String {
    let scalar = |scalar| match scalar {
        ScalarType::Bool => "bool",
        ScalarType::Int => "int",
        ScalarType::Uint => "uint",
        ScalarType::Float => "float",
    };
    match ty {
        Type::Void => "void".to_owned(),
        Type::Scalar(value) => scalar(*value).to_owned(),
        Type::Vector(vector) => format!("{}{}", scalar(vector.scalar), vector.width),
        Type::Matrix(matrix) => format!("float{}x{}", matrix.columns, matrix.rows),
        Type::StructRef(name) => name.clone(),
        Type::Texture(dimension) => match dimension {
            TextureDimension::Texture2d => "texture2d<float>".to_owned(),
            TextureDimension::TextureCube => "texturecube<float>".to_owned(),
            TextureDimension::Depth2d => "depth2d<float>".to_owned(),
            TextureDimension::Texture2dUint => "texture2d<uint>".to_owned(),
        },
        Type::Sampler(_) => "sampler".to_owned(),
        Type::RuntimeArray(element) => format!("device {}*", type_name(element)),
    }
}

/// The attribute a builtin carries in an entry point's interface.
///
/// Metal names a builtin by where it sits, and the same value has a different
/// name per stage — `[[position]]` is an output of a vertex function and
/// `[[position]]` again as a fragment input, but a thread id is
/// `[[thread_position_in_grid]]` and exists only in a kernel.
#[must_use]
pub fn builtin_attribute(builtin: Builtin, stage: Stage, is_input: bool) -> &'static str {
    match builtin {
        Builtin::Position => "[[position]]",
        Builtin::VertexIndex => "[[vertex_id]]",
        Builtin::InstanceIndex => "[[instance_id]]",
        Builtin::FrontFacing => "[[front_facing]]",
        Builtin::FragCoord => "[[position]]",
        Builtin::ThreadId => "[[thread_position_in_grid]]",
        Builtin::LocalId => "[[thread_position_in_threadgroup]]",
        Builtin::GroupId => "[[threadgroup_position_in_grid]]",
        Builtin::LocalIndex => {
            let _ = (stage, is_input);
            "[[thread_index_in_threadgroup]]"
        }
    }
}

/// The running emission: the module being read and the text being built.
pub(crate) struct Emitter<'a> {
    pub(crate) module: &'a CheckedModule,
    pub(crate) reflection: &'a Reflection,
    pub(crate) out: String,
    /// While a stage entry point is being emitted, the name each interface
    /// struct takes there.
    ///
    /// Metal spells a vertex output and a fragment input differently even when
    /// KSL wrote one type, so each stage gets its own struct — and a body that
    /// says `VOut` has to say that stage's name for it instead.
    pub(crate) renames: std::collections::HashMap<String, String>,
}

impl Emitter<'_> {
    /// The MSL spelling of `ty` under the current stage's renames.
    pub(crate) fn spell(&self, ty: &Type) -> String {
        match ty {
            Type::StructRef(name) => self
                .renames
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.clone()),
            other => type_name(other),
        }
    }

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
    fn stmt(&mut self, id: CheckedStmtId, depth: usize) {
        match self.module.stmt(id).clone() {
            CheckedStmt::Let { name, ty, init } => {
                let declared = match init {
                    // A `let` with no initializer is storage the body fills in,
                    // and Metal leaves it uninitialized unless it is zeroed —
                    // which a shader reading a field it never wrote would then
                    // read as garbage.
                    None => format!("{} {name} = {{}};", self.spell(&ty)),
                    Some(value) => {
                        format!("{} {name} = {};", self.spell(&ty), self.expr(value))
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
    ///
    /// Every compound expression is parenthesized rather than tracked by
    /// precedence: MSL's ladder matches KSL's, so the tree already groups
    /// correctly and the parentheses only make that explicit.
    pub(crate) fn expr(&self, id: CheckedExprId) -> String {
        let node = self.module.expr(id);
        match &node.kind {
            CheckedExprKind::Const(value) => constant(*value, &node.ty),
            CheckedExprKind::Local(name) | CheckedExprKind::Option(name) => name.clone(),
            CheckedExprKind::Resource(name) => name.clone(),
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
                // Metal has no way to ask a buffer its length, so the host
                // binds it and the shader reads the parameter instead.
                format!("{}_count", self.expr(*base))
            }
            CheckedExprKind::Index { base, index } => {
                format!("{}[{}]", self.expr(*base), self.expr(*index))
            }
            CheckedExprKind::Construct { args } => {
                let parts = self.args(args);
                format!("{}({parts})", self.spell(&node.ty))
            }
            CheckedExprKind::Cast { value } => {
                format!("{}({})", self.spell(&node.ty), self.expr(*value))
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
            // A rejected expression never reaches a backend: emission runs only
            // on a module that checked clean.
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

    /// Renders a builtin call in Metal's spelling.
    fn builtin(&self, which: BuiltinFn, args: &[CheckedExprId]) -> String {
        let at = |index: usize| {
            args.get(index)
                .map_or_else(String::new, |&id| self.expr(id))
        };
        match which {
            // Metal matrices are column-major and multiply on the left, which
            // is the order KSL writes — so `mul` is the operator here.
            BuiltinFn::Mul => format!("({} * {})", at(0), at(1)),
            BuiltinFn::Sample => format!("{}.sample({}, {})", at(0), at(1), at(2)),
            BuiltinFn::Load => format!("{}.read(uint2({}))", at(0), at(1)),
            BuiltinFn::AtomicAdd => format!(
                "atomic_fetch_add_explicit((device atomic_uint*)&{}[{}], {}, memory_order_relaxed)",
                at(0),
                at(1),
                at(2)
            ),
            BuiltinFn::Fract => format!("fract({})", at(0)),
            BuiltinFn::Atan2 => format!("atan2({}, {})", at(0), at(1)),
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
                    BuiltinFn::Mix => "mix",
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

    /// The parameter list a stage's resources contribute.
    pub(crate) fn resource_params(&self, stage: Stage) -> Vec<String> {
        self.reflection
            .resources
            .iter()
            .filter(|resource| resource.visibility.contains(&stage))
            .flat_map(|resource| self.resource_param(resource, stage))
            .collect()
    }

    /// The parameters one resource contributes, length buffer included.
    ///
    /// # The index is the host's, and it differs per stage
    ///
    /// A graphics host binds a bind-group slot into Metal's buffer table, and
    /// vertex buffer 0 already holds the vertex attribute stream — so a vertex
    /// uniform lands at `slot + 1` while the same uniform in the fragment stage
    /// lands at `slot`, where there is no attribute stream in the way. A shader
    /// that named one index for both stages would read the right buffer in one
    /// and an unbound one in the other, which draws a black frame and reports
    /// nothing. Textures and samplers have tables of their own and take the
    /// slot unchanged.
    ///
    /// The slot is the resource's WGSL binding: that is the number an
    /// application binds against, so it is the one both sides agree on.
    fn resource_param(&self, resource: &ReflectedResource, stage: Stage) -> Vec<String> {
        let Some(binding) = resource
            .backend_bindings
            .iter()
            .find(|binding| binding.target == kira_shader_model::BackendTarget::Wgsl)
        else {
            return Vec::new();
        };
        let slot = binding.binding_index;
        let at = match (resource.resource_kind, stage) {
            (ResourceKind::Uniform | ResourceKind::Storage, Stage::Vertex) => slot + 1,
            _ => slot,
        };
        let name = &resource.resource_name;
        let mut params = match resource.resource_kind {
            ResourceKind::Uniform => vec![format!(
                "constant {}& {name} [[buffer({at})]]",
                resource.type_name
            )],
            ResourceKind::Storage => {
                let element = element_name(&resource.type_name);
                let qualifier = if resource.access == Some(kira_shader_model::AccessMode::ReadWrite)
                {
                    "device"
                } else {
                    "const device"
                };
                vec![format!("{qualifier} {element}* {name} [[buffer({at})]]")]
            }
            ResourceKind::Texture => {
                vec![format!(
                    "{} {name} [[texture({at})]]",
                    texture_name(&resource.type_name)
                )]
            }
            ResourceKind::Sampler => vec![format!("sampler {name} [[sampler({at})]]")],
        };
        for (target, length) in &resource.length_bindings {
            if *target == kira_shader_model::BackendTarget::Msl {
                // Above every slot a bind group can occupy, so a count buffer
                // can never land on one the host is already binding into.
                params.push(format!(
                    "constant uint& {name}_count [[buffer({})]]",
                    COUNT_BUFFER_BASE + length
                ));
            }
        }
        params
    }
}

/// The element type inside a reflected `[T]`.
fn element_name(type_name: &str) -> String {
    type_name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map_or_else(|| type_name.to_owned(), msl_scalar)
}

/// The MSL spelling of a KSL type written as a name.
fn msl_scalar(name: &str) -> String {
    match name {
        "Float" => "float",
        "UInt" => "uint",
        "Int" => "int",
        "Bool" => "bool",
        "Float2" => "float2",
        "Float3" => "float3",
        "Float4" => "float4",
        "UInt2" => "uint2",
        "UInt3" => "uint3",
        "UInt4" => "uint4",
        "Int2" => "int2",
        "Int3" => "int3",
        "Int4" => "int4",
        "Float4x4" => "float4x4",
        "Float3x3" => "float3x3",
        "Float2x2" => "float2x2",
        other => other,
    }
    .to_owned()
}

/// The MSL texture type a reflected texture name spells.
fn texture_name(name: &str) -> &'static str {
    match name {
        "Texture2dUint" => "texture2d<uint>",
        "TextureCube" => "texturecube<float>",
        "Depth2d" => "depth2d<float>",
        _ => "texture2d<float>",
    }
}

/// A constant, written so its type survives.
fn constant(value: ConstValue, ty: &Type) -> String {
    match value {
        ConstValue::Bool(value) => value.to_string(),
        ConstValue::Int(value) => value.to_string(),
        ConstValue::Uint(value) => format!("{value}u"),
        // `1.0` rather than `1`, or Metal would read it as an int and an
        // integer division would silently truncate.
        ConstValue::Float(value) => {
            let _ = ty;
            if value.fract() == 0.0 && value.is_finite() {
                format!("{value:.1}")
            } else {
                format!("{value:?}")
            }
        }
    }
}

/// How a binary operator is written in MSL.
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
