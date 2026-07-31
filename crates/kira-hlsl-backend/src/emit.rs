//! Emitting HLSL: types, semantics, expressions, statements, and resources.

use std::collections::HashMap;

use kira_ksl_semantics::model::{
    BinaryOp, BuiltinFn, CheckedExprId, CheckedExprKind, CheckedFunction, CheckedModule,
    CheckedStmt, CheckedStmtId, ConstValue, UnaryOp,
};
use kira_shader_model::{
    AccessMode, BackendTarget, Builtin, Interpolation, Reflection, ResourceKind, ScalarType, Stage,
    TextureDimension, Type,
};

/// The HLSL spelling of a type.
///
/// A matrix is transposed in the spelling and only in the spelling: HLSL writes
/// `floatRxC` for R rows and C columns where every other dialect here writes
/// columns first. The matrix itself is the same one — see [`matrix_storage`]
/// for the half of that agreement which is about bytes rather than names.
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
        Type::Matrix(matrix) => format!("float{}x{}", matrix.rows, matrix.columns),
        Type::StructRef(name) => name.clone(),
        Type::Texture(dimension) => match dimension {
            TextureDimension::Texture2d => "Texture2D<float4>".to_owned(),
            TextureDimension::TextureCube => "TextureCube<float4>".to_owned(),
            // A depth texture is one channel, and comparison sampling reads it
            // as a scalar rather than as the first lane of a four-vector.
            TextureDimension::Depth2d => "Texture2D<float>".to_owned(),
            TextureDimension::Texture2dUint => "Texture2D<uint4>".to_owned(),
        },
        Type::Sampler(kind) => match kind {
            kira_shader_model::SamplerKind::Filtering => "SamplerState".to_owned(),
            kira_shader_model::SamplerKind::Comparison => "SamplerComparisonState".to_owned(),
        },
        Type::RuntimeArray(element) => format!("StructuredBuffer<{}>", type_name(element)),
    }
}

/// The qualifier a matrix-typed declaration carries.
///
/// This workspace lays a matrix out as its column count of 4-wide vectors,
/// because that is `std140`'s rule and the host packs to it. HLSL's default is
/// the other one — it stores rows — so a matrix declared without this reads the
/// host's bytes transposed, which is a wrong image rather than a compile error.
#[must_use]
pub fn matrix_storage(ty: &Type) -> &'static str {
    match ty {
        Type::Matrix(_) => "column_major ",
        _ => "",
    }
}

/// The semantic a builtin carries in a stage interface.
#[must_use]
pub fn semantic(builtin: Builtin) -> &'static str {
    match builtin {
        Builtin::Position | Builtin::FragCoord => "SV_Position",
        Builtin::VertexIndex => "SV_VertexID",
        Builtin::InstanceIndex => "SV_InstanceID",
        Builtin::FrontFacing => "SV_IsFrontFace",
        Builtin::ThreadId => "SV_DispatchThreadID",
        Builtin::LocalId => "SV_GroupThreadID",
        Builtin::GroupId => "SV_GroupID",
        Builtin::LocalIndex => "SV_GroupIndex",
    }
}

/// The interpolation modifier a varying carries, empty for the default.
#[must_use]
pub fn interpolation_modifier(interpolation: Option<Interpolation>) -> &'static str {
    match interpolation {
        Some(Interpolation::Flat) => "nointerpolation ",
        Some(Interpolation::Linear) => "noperspective ",
        _ => "",
    }
}

/// The running emission.
pub(crate) struct Emitter<'a> {
    pub(crate) module: &'a CheckedModule,
    pub(crate) reflection: &'a Reflection,
    pub(crate) out: String,
    /// While a stage entry point is being emitted, the name each interface
    /// struct takes there.
    ///
    /// An interface struct is emitted a second time carrying this stage's
    /// semantics, and a body that says `VOut` has to say that copy's name.
    pub(crate) renames: HashMap<String, String>,
    /// Statements the expression being rendered needs run before it.
    ///
    /// HLSL has no expression form for an atomic or for a buffer's length —
    /// `InterlockedAdd` and `GetDimensions` both answer through `out`
    /// parameters — so those become a temporary declared and filled here, and
    /// the expression renders as the temporary's name.
    pending: Vec<String>,
    /// How many temporaries the current function has already taken.
    temporaries: u32,
}

impl<'a> Emitter<'a> {
    /// A fresh emitter over `module` and its `reflection`.
    pub(crate) fn new(module: &'a CheckedModule, reflection: &'a Reflection) -> Self {
        Self {
            module,
            reflection,
            out: String::new(),
            renames: HashMap::new(),
            pending: Vec::new(),
            temporaries: 0,
        }
    }

    /// The HLSL spelling of `ty` under the current stage's renames.
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

    /// Writes one statement, and whatever its expressions had to run first.
    pub(crate) fn stmt(&mut self, id: CheckedStmtId, depth: usize) {
        match self.module.stmt(id).clone() {
            CheckedStmt::Let { name, ty, init } => {
                let declared = match init {
                    // A `let` with no initializer is storage the body fills in.
                    // HLSL leaves it uninitialized, and a field the body never
                    // wrote would then be read as whatever the register held.
                    None => format!("{} {name} = ({})0;", self.spell(&ty), self.spell(&ty)),
                    Some(value) => {
                        let rendered = self.expr(value);
                        format!("{} {name} = {rendered};", self.spell(&ty))
                    }
                };
                self.flush(depth);
                self.line(depth, &declared);
            }
            CheckedStmt::Assign { target, value } => {
                let target = self.expr(target);
                let value = self.expr(value);
                self.flush(depth);
                let assignment = format!("{target} = {value};");
                self.line(depth, &assignment);
            }
            CheckedStmt::If {
                cond,
                then,
                otherwise,
            } => {
                let cond = self.expr(cond);
                self.flush(depth);
                let opened = format!("if ({cond}) {{");
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
            // A loop condition is re-evaluated every iteration, so nothing in
            // it may be hoisted out. `refuse` rejects the shader before this
            // runs rather than emitting a loop that reads a stale temporary.
            CheckedStmt::While { cond, body } => {
                let cond = self.expr(cond);
                self.flush(depth);
                let opened = format!("while ({cond}) {{");
                self.line(depth, &opened);
                self.body(&body, depth + 1);
                self.line(depth, "}");
            }
            CheckedStmt::Return(None) => self.line(depth, "return;"),
            CheckedStmt::Return(Some(value)) => {
                let value = self.expr(value);
                self.flush(depth);
                let returned = format!("return {value};");
                self.line(depth, &returned);
            }
            CheckedStmt::Expr(value) => {
                let rendered = self.expr(value);
                self.flush(depth);
                // An atomic in statement position rendered to its temporary and
                // did its work in the hoisted lines; writing the temporary out
                // again would be a statement with no effect.
                if !rendered.starts_with(TEMPORARY) {
                    let evaluated = format!("{rendered};");
                    self.line(depth, &evaluated);
                }
            }
        }
    }

    /// Writes and clears whatever the last expression hoisted.
    fn flush(&mut self, depth: usize) {
        for line in std::mem::take(&mut self.pending) {
            self.line(depth, &line);
        }
    }

    /// The name of a fresh temporary.
    fn temporary(&mut self) -> String {
        let at = self.temporaries;
        self.temporaries += 1;
        format!("{TEMPORARY}{at}")
    }

    /// Renders one expression, hoisting whatever HLSL cannot say inline.
    pub(crate) fn expr(&mut self, id: CheckedExprId) -> String {
        let node = self.module.expr(id).clone();
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
            // `GetDimensions` answers through `out` parameters, so the length
            // is read into a temporary before the statement that wanted it.
            CheckedExprKind::ArrayLength { base } => {
                let buffer = self.expr(*base);
                let count = self.temporary();
                let stride = self.temporary();
                self.pending.push(format!("uint {count};"));
                self.pending.push(format!("uint {stride};"));
                self.pending
                    .push(format!("{buffer}.GetDimensions({count}, {stride});"));
                count
            }
            CheckedExprKind::Index { base, index } => {
                format!("{}[{}]", self.expr(*base), self.expr(*index))
            }
            CheckedExprKind::Construct { args } => {
                let args = self.args(args);
                format!("{}({args})", self.spell(&node.ty))
            }
            CheckedExprKind::Cast { value } => {
                let value = self.expr(*value);
                format!("({}){value}", self.spell(&node.ty))
            }
            CheckedExprKind::Call { name, args } => {
                let args = self.args(args);
                format!("{name}({args})")
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
                let lhs = self.expr(*lhs);
                let rhs = self.expr(*rhs);
                format!("({lhs} {} {rhs})", binary_spelling(*op))
            }
            CheckedExprKind::Invalid => "0".to_owned(),
        }
    }

    /// Renders a comma-separated argument list.
    fn args(&mut self, args: &[CheckedExprId]) -> String {
        args.iter()
            .map(|&arg| self.expr(arg))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Renders a builtin call in HLSL's spelling.
    fn builtin(&mut self, which: BuiltinFn, args: &[CheckedExprId]) -> String {
        let rendered: Vec<String> = args.iter().map(|&arg| self.expr(arg)).collect();
        let at = |index: usize| rendered.get(index).cloned().unwrap_or_default();
        match which {
            BuiltinFn::Mul => format!("mul({}, {})", at(0), at(1)),
            BuiltinFn::Sample => format!("{}.Sample({}, {})", at(0), at(1), at(2)),
            // `Load` takes the mip level as the last coordinate component; KSL's
            // `load` names no level, so level 0 is implied.
            BuiltinFn::Load => format!("{}.Load(int3({}, 0))", at(0), at(1)),
            // The one builtin that is a statement in HLSL: it answers the value
            // that was there through an `out` parameter rather than by
            // returning it, which is what the corpus's `let slot = atomicAdd(…)`
            // wants.
            BuiltinFn::AtomicAdd => {
                let previous = self.temporary();
                self.pending.push(format!("uint {previous};"));
                self.pending.push(format!(
                    "InterlockedAdd({}[{}], {}, {previous});",
                    at(0),
                    at(1),
                    at(2)
                ));
                previous
            }
            BuiltinFn::Fract => format!("frac({})", at(0)),
            BuiltinFn::Mix => format!("lerp({}, {}, {})", at(0), at(1), at(2)),
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
                format!("{name}({})", rendered.join(", "))
            }
        }
    }

    /// Writes every resource as a module-scope declaration with its register.
    ///
    /// # Why a read-write buffer's register looks sparse
    ///
    /// D3D addresses a read-only resource in the `t` space and a read-write one
    /// in `u`, and the IR numbers both out of one counter — so a shader holding
    /// a texture and a read-write buffer emits `t0` and `u1`, leaving `u0`
    /// unused. That is deliberate. The number is the one the reflection carries,
    /// so a host binds the slot it was told about; making the spaces count
    /// independently would save a slot nothing was going to use and give two
    /// resources the same number to be confused by.
    pub(crate) fn resources(&mut self) {
        let resources = self.reflection.resources.clone();
        for resource in &resources {
            let Some(binding) = resource
                .backend_bindings
                .iter()
                .find(|binding| binding.target == BackendTarget::Hlsl)
            else {
                continue;
            };
            let at = binding.binding_index;
            let name = &resource.resource_name;
            match resource.resource_kind {
                // The struct itself sits inside the buffer, so the body's
                // `camera.view_projection` reads the same path it wrote.
                ResourceKind::Uniform => {
                    let opened = format!("cbuffer {name}_buffer : register(b{at}) {{");
                    self.line(0, &opened);
                    let declared = format!("    {} {name};", resource.type_name);
                    self.line(0, &declared);
                    self.line(0, "};");
                }
                ResourceKind::Storage => {
                    let element = element_name(&resource.type_name);
                    let declared = if resource.access == Some(AccessMode::ReadWrite) {
                        format!("RWStructuredBuffer<{element}> {name} : register(u{at});")
                    } else {
                        format!("StructuredBuffer<{element}> {name} : register(t{at});")
                    };
                    self.line(0, &declared);
                }
                ResourceKind::Texture => {
                    let declared = format!(
                        "{} {name} : register(t{at});",
                        texture_type(&resource.type_name)
                    );
                    self.line(0, &declared);
                }
                ResourceKind::Sampler => {
                    let declared = format!(
                        "{} {name} : register(s{at});",
                        sampler_type(&resource.type_name)
                    );
                    self.line(0, &declared);
                }
            }
        }
        if !resources.is_empty() {
            self.out.push('\n');
        }
    }
}

/// The prefix every hoisted temporary's name carries.
///
/// Long enough that a shader cannot collide with it: KSL's identifiers are the
/// author's, and `kira_` names are this backend's.
const TEMPORARY: &str = "kira_temporary_";

/// The element type inside a reflected `[T]`.
fn element_name(type_name: &str) -> String {
    type_name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map_or_else(|| type_name.to_owned(), hlsl_name)
}

/// The HLSL spelling of a KSL type written as a name.
#[must_use]
pub fn hlsl_name(name: &str) -> String {
    kira_ksl_semantics::builtins::builtin_type(name)
        .map_or_else(|| name.to_owned(), |ty| type_name(&ty))
}

/// The HLSL texture type a reflected texture name spells.
fn texture_type(name: &str) -> &'static str {
    match name {
        "Texture2dUint" => "Texture2D<uint4>",
        "TextureCube" => "TextureCube<float4>",
        // A depth texture is one channel, and comparison sampling reads it as
        // a scalar rather than as the first lane of a four-vector.
        "Depth2d" => "Texture2D<float>",
        _ => "Texture2D<float4>",
    }
}

/// The HLSL sampler type a reflected sampler name spells.
fn sampler_type(name: &str) -> &'static str {
    match name {
        "SamplerComparison" => "SamplerComparisonState",
        _ => "SamplerState",
    }
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

/// How a binary operator is written in HLSL.
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

/// The attribute a compute entry point carries.
#[must_use]
pub fn threads_attribute(stage: Stage, threads: Option<[u32; 3]>) -> String {
    match stage {
        Stage::Compute => {
            let [x, y, z] = threads.unwrap_or([1, 1, 1]);
            format!("[numthreads({x}, {y}, {z})]\n")
        }
        _ => String::new(),
    }
}
