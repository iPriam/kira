//! Emitting GLSL 430: types, expressions, and statements.

use kira_ksl_semantics::model::{
    BinaryOp, BuiltinFn, CheckedExprId, CheckedExprKind, CheckedFunction, CheckedModule,
    CheckedStmt, CheckedStmtId, ConstValue, UnaryOp,
};
use kira_shader_model::{Builtin, ReflectedField, Reflection, ScalarType, Stage, Type};

// The one function that decides a resource's emitted GLSL name. It lives in the
// IR because the reflection reports the same string as `glsl_name`, and a host
// binds by it.
pub(crate) use kira_shader_ir::glsl_safe_name as safe_name;

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
        // GLSL has no standalone sampler object, so one never reaches a
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
    /// The stage this module is, which decides which resources it declares.
    pub(crate) stage: Stage,
    /// The sampler names folded into a texture, which never get a declaration.
    pub(crate) samplers: Vec<String>,
    /// The textures declared as images rather than samplers, because the shader
    /// writes them. `load` and `store` reach an image through `imageLoad` and
    /// `imageStore`, so the emitter has to know which of the two a name is.
    pub(crate) images: Vec<String>,
    /// How the entry point's `return value` becomes a copy-out, while emitting
    /// its body.
    ///
    /// `main` returns nothing and a stage's outputs are variables, so every
    /// `return value` in the entry becomes assignments plus a bare `return` —
    /// including the ones inside an `if`, which is why this is a mode the
    /// statement emitter reads rather than a rewrite of the body's top level.
    pub(crate) entry_outputs: Option<EntryOutputs>,
}

/// What the entry point's returns copy into.
pub(crate) struct EntryOutputs {
    /// The stage's outputs, in reflection order.
    pub(crate) fields: Vec<ReflectedField>,
    /// The stage being emitted, which decides how an output is named.
    pub(crate) stage: Stage,
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
    ///
    /// A `Sampler` parameter does not survive. GLSL has no standalone sampler
    /// object, so there is no type to spell one with — a helper that took a
    /// texture and the sampler reading it emitted a parameter with an empty type
    /// and did not compile. The texture parameter already carries the sampling
    /// state, and [`Self::args`] drops the matching argument at every call, which
    /// is the same rule `sample` itself follows.
    pub(crate) fn function(&mut self, function: &CheckedFunction) {
        let params = function
            .params
            .iter()
            .filter(|param| !matches!(param.ty, Type::Sampler(_)))
            .map(|param| format!("{} {}", type_name(&param.ty), safe_name(&param.name)))
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
                let name = safe_name(&name);
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
            CheckedStmt::Return(Some(value)) if self.entry_outputs.is_some() => {
                let returned = self.expr(value);
                let outputs = self.entry_outputs.take().expect("checked by the guard");
                for field in &outputs.fields {
                    let target = match (field.builtin, outputs.stage) {
                        (Some(builtin), stage) => builtin_name(builtin, stage).to_owned(),
                        (None, Stage::Fragment) => field.name.clone(),
                        (None, _) => format!("v_{}", field.name),
                    };
                    let line = format!("{target} = {returned}.{};", field.name);
                    self.line(depth, &line);
                }
                self.entry_outputs = Some(outputs);
                self.line(depth, "return;");
            }
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
            | CheckedExprKind::Resource(name) => safe_name(name),
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

    /// Whether `arg` names a texture this shader writes, and so was declared as
    /// an image rather than a sampler.
    fn is_image(&self, arg: Option<CheckedExprId>) -> bool {
        let Some(id) = arg else {
            return false;
        };
        match &self.module.expr(id).kind {
            CheckedExprKind::Resource(name) | CheckedExprKind::Local(name) => {
                self.images.contains(&safe_name(name))
            }
            _ => false,
        }
    }

    /// Renders a comma-separated argument list, less the samplers.
    ///
    /// A sampler is folded into the texture it reads and has no GLSL type, so
    /// the parameter it would bind to is not there — see [`Self::function`].
    fn args(&self, args: &[CheckedExprId]) -> String {
        args.iter()
            .filter(|&&arg| !matches!(self.module.expr(arg).ty, Type::Sampler(_)))
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
            // The sampler argument has no GLSL counterpart: the texture
            // uniform already carries the sampling state, so it is dropped.
            BuiltinFn::Sample => format!("texture({}, {})", at(0), at(2)),
            // A sampled texture is fetched by level; an image has no levels and
            // is read through `imageLoad` instead.
            BuiltinFn::Load if self.is_image(args.first().copied()) => {
                format!("imageLoad({}, ivec2({}))", at(0), at(1))
            }
            BuiltinFn::Load => format!("texelFetch({}, ivec2({}), 0)", at(0), at(1)),
            BuiltinFn::Store => format!("imageStore({}, ivec2({}), {})", at(0), at(1), at(2)),
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

    /// Writes the resources this stage reads as uniform declarations.
    ///
    /// A resource the stage never names is left out, exactly as the MSL
    /// backend leaves it out of the entry point's parameter list. It is not
    /// merely redundant: a GL driver strips an unused global, so the uniform
    /// the host then looks up by name is not in the linked program, and
    /// `glGetUniformLocation` answers -1. The host reports that as a mismatch
    /// between the shader and the bindings — which is what it would be, if the
    /// declaration had been load-bearing.
    pub(crate) fn resources(&mut self) {
        let resources = self.reflection.resources.clone();
        let stage = self.stage;
        let mut wrote = false;
        for resource in resources
            .iter()
            .filter(|resource| resource.visibility.contains(&stage))
        {
            // The SAME name the reflection reports as this resource's
            // `glsl_name`, because that is what a GL host binds by — see
            // `glsl_safe_name`.
            let name = safe_name(&resource.resource_name);
            match resource.resource_kind {
                kira_shader_model::ResourceKind::Uniform => {
                    // A struct-typed uniform, not an interface block.
                    //
                    // A block would be the modern spelling, and it is the wrong
                    // one here: a GL host sets a block's contents through a
                    // buffer object it has to bind, while every member of a
                    // struct uniform has an ordinary location that
                    // `glGetUniformLocation("params.width")` finds. The graphics
                    // hosts that consume this — sokol_gfx among them — address
                    // uniforms by that dotted name and have no UBO path at all,
                    // so a block reaches the driver fine and then nothing ever
                    // writes to it: the shader compiles, the draw goes out, and
                    // every uniform reads zero.
                    //
                    // The body still writes `params.field` either way, so this
                    // is a declaration change and nothing else. The struct itself
                    // is emitted with the module's other types.
                    let line = format!("uniform {} {name};", resource.type_name);
                    self.line(0, &line);
                    wrote = true;
                }
                kira_shader_model::ResourceKind::Texture => {
                    // A written texture is an IMAGE in GLSL, not a sampler:
                    // `imageStore` takes an `image2D` bound to an image unit,
                    // and a `sampler2D` cannot be written at all. Its unit is
                    // the binding index reflection gave this target, and its
                    // format has to be spelled in the declaration because GLSL
                    // resolves the store's texel conversion from it.
                    let line = match storage_access(resource.access) {
                        Some(access) => {
                            let binding = binding_index(resource);
                            self.images.push(name.clone());
                            format!(
                                "layout(binding = {binding}, {}) uniform {access}{} {name};",
                                image_format(&resource.type_name),
                                image_name(&resource.type_name)
                            )
                        }
                        None => format!("uniform {} {name};", texture_name(&resource.type_name)),
                    };
                    self.line(0, &line);
                    wrote = true;
                }
                // A sampler folds into the texture it reads and never becomes
                // a declaration of its own.
                kira_shader_model::ResourceKind::Sampler => {
                    self.samplers.push(name.clone());
                }
                // A shader storage block, bound at the index reflection gave
                // it. The unsized trailing array is what makes the length a
                // run-time property, which is the whole reason a storage buffer
                // is not a uniform block.
                kira_shader_model::ResourceKind::Storage => {
                    let binding = binding_index(resource);
                    let element = element_name(&resource.type_name);
                    let readonly = if resource.access == Some(kira_shader_model::AccessMode::Read) {
                        "readonly "
                    } else {
                        ""
                    };
                    let opened = format!(
                        "layout(std430, binding = {binding}) {readonly}buffer {name}_block {{"
                    );
                    self.line(0, &opened);
                    let field = format!("{element} {name}[];");
                    self.line(1, &field);
                    self.line(0, "};");
                    wrote = true;
                }
            }
        }
        if wrote {
            self.out.push('\n');
        }
    }
}

/// The GLSL element type a storage binding's reflected name describes.
///
/// Reflection spells the binding in KSL — `[Float]` — and the block declares the
/// unsized array around the element, so the brackets come off and what is left
/// is translated the way any written type is. A name that is not a KSL scalar is
/// a struct, whose GLSL name is its own.
fn element_name(name: &str) -> String {
    let inner = name
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(name);
    match inner {
        "Bool" => "bool".to_owned(),
        "Int" => "int".to_owned(),
        "UInt" | "Uint" => "uint".to_owned(),
        "Float" => "float".to_owned(),
        "Float2" => "vec2".to_owned(),
        "Float3" => "vec3".to_owned(),
        "Float4" => "vec4".to_owned(),
        "Int2" => "ivec2".to_owned(),
        "Int3" => "ivec3".to_owned(),
        "Int4" => "ivec4".to_owned(),
        "UInt2" | "Uint2" => "uvec2".to_owned(),
        "UInt3" | "Uint3" => "uvec3".to_owned(),
        "UInt4" | "Uint4" => "uvec4".to_owned(),
        other => other.to_owned(),
    }
}

/// The binding index this target assigned a resource, or 0 when it assigned
/// none — the same rule for a storage block and for an image unit.
fn binding_index(resource: &kira_shader_model::ReflectedResource) -> u32 {
    resource
        .backend_bindings
        .iter()
        .find(|binding| binding.target == kira_shader_model::BackendTarget::Glsl430)
        .map_or(0, |binding| binding.binding_index)
}

/// The GLSL access qualifier a written texture is declared with, or `None` when
/// the texture is only read and stays a sampler.
///
/// A read-write image carries no qualifier at all — that is GLSL's default —
/// so the spelling includes its own trailing space when there is one to write.
fn storage_access(access: Option<kira_shader_model::AccessMode>) -> Option<&'static str> {
    match access {
        Some(kira_shader_model::AccessMode::Write) => Some("writeonly "),
        Some(kira_shader_model::AccessMode::ReadWrite) => Some(""),
        Some(kira_shader_model::AccessMode::Read) | None => None,
    }
}

/// The GLSL image type a written texture's reflected name spells.
fn image_name(name: &str) -> &'static str {
    match name {
        "Texture2dUint" => "uimage2D",
        _ => "image2D",
    }
}

/// The format qualifier a written texture is declared with.
///
/// GLSL demands the texel format on an image, which KSL does not say. The
/// default is the one a colour target has — the same choice WGSL's storage
/// texture makes, so the two targets write the same texels.
fn image_format(name: &str) -> &'static str {
    match name {
        "Texture2dUint" => "r32ui",
        _ => "rgba8",
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
