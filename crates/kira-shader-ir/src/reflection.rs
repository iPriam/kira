//! `KSLR1`: the serialized reflection a graphics host binds against.
//!
//! # The format
//!
//! UTF-8 text, one record per line, fields separated by single spaces. The
//! first line is the magic `KSLR1`. Every following line opens with a keyword
//! naming its record:
//!
//! ```text
//! KSLR1
//! shader Tri graphics msl
//! option use_tint Bool true
//! stage vertex entry vertex_main input VertexIn output VertexOut
//! threads 64 1 1
//! in vertex 0 position Float2 - -
//! out vertex 0 color Float4 - flat
//! resource Frame frame 0 camera uniform Camera read vertex,fragment
//! bind camera msl 0 1 -
//! layout Camera 16 80
//! field Camera view_projection 0 16 64 64
//! count particles msl 3
//! ```
//!
//! Text rather than a packed binary because a host reads this once at pipeline
//! creation, never in a frame, and a format a person can read in a build log is
//! worth more there than the bytes it saves. Line-oriented rather than JSON so
//! a reader needs no dependency to parse it.
//!
//! # Compatibility
//!
//! The magic is versioned and the record set is **append-only**: a new keyword
//! goes on the end of the grammar and never changes an existing record's field
//! order or count. A decoder rejects an unknown keyword rather than skipping
//! it, because a host that silently ignored a binding it did not understand
//! would draw with the wrong resources rather than fail.
//!
//! A `-` stands for an absent optional field, so every record of one kind has
//! the same field count and a reader can split on spaces without counting.

use kira_shader_model::{
    AccessMode, BackendBinding, BackendTarget, Builtin, GroupClass, Interpolation, ReflectedField,
    ReflectedLayout, ReflectedLayoutField, ReflectedOption, ReflectedResource, ReflectedStage,
    ReflectedType, Reflection, ResourceKind, ShaderKind, Stage,
};

/// The magic opening a `KSLR1` document.
pub const MAGIC: &str = "KSLR1";

/// Why a reflection document could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReflectionError {
    /// The document did not open with the magic.
    #[error("expected the `{MAGIC}` magic, found `{found}`")]
    BadMagic {
        /// The first line as written.
        found: String,
    },
    /// A line opened with a keyword the format does not define.
    #[error("line {line}: `{keyword}` is not a record this format defines")]
    UnknownRecord {
        /// The 1-based line number.
        line: usize,
        /// The keyword as written.
        keyword: String,
    },
    /// A line had the wrong number of fields for its keyword.
    #[error("line {line}: `{keyword}` takes {wanted} fields, found {found}")]
    FieldCount {
        /// The 1-based line number.
        line: usize,
        /// The keyword as written.
        keyword: String,
        /// How many fields the record takes.
        wanted: usize,
        /// How many were written.
        found: usize,
    },
    /// A field did not hold the value its position requires.
    #[error("line {line}: `{value}` is not a valid {expected}")]
    BadField {
        /// The 1-based line number.
        line: usize,
        /// The field as written.
        value: String,
        /// What the position needs.
        expected: &'static str,
    },
    /// A record referred to something no earlier record declared.
    #[error("line {line}: `{name}` was not declared before it was referred to")]
    Unanchored {
        /// The 1-based line number.
        line: usize,
        /// The name as written.
        name: String,
    },
}

/// Renders `reflection` as a `KSLR1` document.
#[must_use]
pub fn encode(reflection: &Reflection) -> String {
    let mut out = String::new();
    out.push_str(MAGIC);
    out.push('\n');
    out.push_str(&format!(
        "shader {} {} {}\n",
        reflection.shader_name,
        shader_kind_word(reflection.shader_kind),
        reflection.backend.label()
    ));
    for option in &reflection.options {
        out.push_str(&format!(
            "option {} {} {}\n",
            option.name, option.type_name, option.default_value
        ));
    }
    for stage in &reflection.stages {
        out.push_str(&format!(
            "stage {} entry {} input {} output {}\n",
            stage_word(stage.stage),
            stage.entry_name,
            optional(stage.input_type.as_deref()),
            optional(stage.output_type.as_deref())
        ));
        if let Some([x, y, z]) = stage.threads {
            out.push_str(&format!("threads {x} {y} {z}\n"));
        }
        for field in &stage.inputs {
            out.push_str(&interface_line("in", stage.stage, field));
        }
        for field in &stage.outputs {
            out.push_str(&interface_line("out", stage.stage, field));
        }
    }
    for resource in &reflection.resources {
        out.push_str(&format!(
            "resource {} {} {} {} {} {} {} {}\n",
            resource.group_name,
            group_class_word(resource.group_class),
            resource.group_index,
            resource.resource_name,
            resource_kind_word(resource.resource_kind),
            resource.type_name,
            resource.access.map_or("-", access_word),
            visibility(&resource.visibility),
        ));
        for binding in &resource.backend_bindings {
            out.push_str(&format!(
                "bind {} {} {} {} {}\n",
                resource.resource_name,
                binding.target.label(),
                binding.group_index,
                binding.binding_index,
                optional(binding.glsl_name.as_deref())
            ));
        }
        if let Some(sampler) = &resource.paired_sampler {
            out.push_str(&format!("pair {} {}\n", resource.resource_name, sampler));
        }
        for (target, binding) in &resource.length_bindings {
            out.push_str(&format!(
                "count {} {} {}\n",
                resource.resource_name,
                target.label(),
                binding
            ));
        }
    }
    for declared in &reflection.types {
        for field in &declared.fields {
            out.push_str(&format!(
                "member {} {} {} {} {}\n",
                declared.name,
                field.name,
                field.type_name,
                field.builtin.map_or("-", builtin_word),
                field.interpolation.map_or("-", interpolation_word),
            ));
        }
        if let Some(layout) = &declared.uniform_layout {
            out.push_str(&layout_lines(&declared.name, layout));
        }
    }
    out
}

/// The `in`/`out` record for one interface field.
fn interface_line(keyword: &str, stage: Stage, field: &ReflectedField) -> String {
    format!(
        "{keyword} {} {} {} {} {} {}\n",
        stage_word(stage),
        field
            .location
            .map_or_else(|| "-".to_owned(), |at| at.to_string()),
        field.name,
        field.type_name,
        field.builtin.map_or("-", builtin_word),
        field.interpolation.map_or("-", interpolation_word),
    )
}

/// The `layout` record and its `field` records.
fn layout_lines(name: &str, layout: &ReflectedLayout) -> String {
    let mut out = format!("layout {name} {} {}\n", layout.alignment, layout.size);
    for field in &layout.fields {
        out.push_str(&format!(
            "field {name} {} {} {} {} {}\n",
            field.name, field.offset, field.alignment, field.size, field.stride
        ));
    }
    out
}

/// Decodes a `KSLR1` document.
///
/// Validating: every malformed input is a typed error and none is a panic, so
/// a host handed a corrupt blob refuses to build a pipeline rather than binding
/// something it guessed.
pub fn decode(text: &str) -> Result<Reflection, ReflectionError> {
    let mut lines = text.lines().enumerate();
    let Some((_, magic)) = lines.next() else {
        return Err(ReflectionError::BadMagic {
            found: String::new(),
        });
    };
    if magic.trim_end() != MAGIC {
        return Err(ReflectionError::BadMagic {
            found: magic.to_owned(),
        });
    }

    let mut reflection = Reflection {
        shader_name: String::new(),
        shader_kind: ShaderKind::Graphics,
        backend: BackendTarget::Msl,
        options: Vec::new(),
        stages: Vec::new(),
        types: Vec::new(),
        resources: Vec::new(),
    };
    let mut seen_shader = false;

    for (index, raw) in lines {
        let line = raw.trim_end();
        if line.is_empty() {
            continue;
        }
        let at = index + 1;
        let fields: Vec<&str> = line.split(' ').collect();
        let keyword = fields[0];
        let count = |wanted: usize| -> Result<(), ReflectionError> {
            if fields.len() == wanted {
                Ok(())
            } else {
                Err(ReflectionError::FieldCount {
                    line: at,
                    keyword: keyword.to_owned(),
                    wanted,
                    found: fields.len(),
                })
            }
        };
        match keyword {
            "shader" => {
                count(4)?;
                reflection.shader_name = fields[1].to_owned();
                reflection.shader_kind = parse(shader_kind, fields[2], at, "shader kind")?;
                reflection.backend = parse(BackendTarget::parse, fields[3], at, "backend")?;
                seen_shader = true;
            }
            "option" => {
                count(4)?;
                reflection.options.push(ReflectedOption {
                    name: fields[1].to_owned(),
                    type_name: fields[2].to_owned(),
                    default_value: fields[3].to_owned(),
                });
            }
            "stage" => {
                count(8)?;
                reflection.stages.push(ReflectedStage {
                    stage: parse(stage_from, fields[1], at, "stage")?,
                    entry_name: fields[3].to_owned(),
                    input_type: from_optional(fields[5]),
                    output_type: from_optional(fields[7]),
                    threads: None,
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                });
            }
            "threads" => {
                count(4)?;
                let stage = last_stage(&mut reflection, at)?;
                stage.threads = Some([
                    parse_number(fields[1], at)?,
                    parse_number(fields[2], at)?,
                    parse_number(fields[3], at)?,
                ]);
            }
            "in" | "out" => {
                count(7)?;
                let field = ReflectedField {
                    name: fields[3].to_owned(),
                    type_name: fields[4].to_owned(),
                    builtin: from_optional(fields[5])
                        .map(|word| parse(builtin_from, &word, at, "builtin"))
                        .transpose()?,
                    interpolation: from_optional(fields[6])
                        .map(|word| parse(interpolation_from, &word, at, "interpolation"))
                        .transpose()?,
                    location: match fields[2] {
                        "-" => None,
                        written => Some(parse_number(written, at)?),
                    },
                };
                let stage = last_stage(&mut reflection, at)?;
                if keyword == "in" {
                    stage.inputs.push(field);
                } else {
                    stage.outputs.push(field);
                }
            }
            "resource" => {
                count(9)?;
                reflection.resources.push(ReflectedResource {
                    group_name: fields[1].to_owned(),
                    group_class: parse(group_class_from, fields[2], at, "group class")?,
                    group_index: parse_number(fields[3], at)?,
                    resource_name: fields[4].to_owned(),
                    resource_kind: parse(resource_kind_from, fields[5], at, "resource kind")?,
                    type_name: fields[6].to_owned(),
                    access: from_optional(fields[7])
                        .map(|word| parse(access_from, &word, at, "access mode"))
                        .transpose()?,
                    visibility: parse_visibility(fields[8], at)?,
                    backend_bindings: Vec::new(),
                    length_bindings: Vec::new(),
                    paired_sampler: None,
                });
            }
            "pair" => {
                count(3)?;
                let sampler = fields[2].to_owned();
                let owner = reflection
                    .resources
                    .iter_mut()
                    .find(|resource| resource.resource_name == fields[1])
                    .ok_or_else(|| ReflectionError::Unanchored {
                        line: at,
                        name: fields[1].to_owned(),
                    })?;
                owner.paired_sampler = Some(sampler);
            }
            "bind" => {
                count(6)?;
                let binding = BackendBinding {
                    target: parse(BackendTarget::parse, fields[2], at, "backend")?,
                    group_index: parse_number(fields[3], at)?,
                    binding_index: parse_number(fields[4], at)?,
                    glsl_name: from_optional(fields[5]),
                };
                let owner = reflection
                    .resources
                    .iter_mut()
                    .find(|resource| resource.resource_name == fields[1])
                    .ok_or_else(|| ReflectionError::Unanchored {
                        line: at,
                        name: fields[1].to_owned(),
                    })?;
                owner.backend_bindings.push(binding);
            }
            "member" => {
                count(6)?;
                let field = ReflectedField {
                    name: fields[2].to_owned(),
                    type_name: fields[3].to_owned(),
                    builtin: from_optional(fields[4])
                        .map(|word| parse(builtin_from, &word, at, "builtin"))
                        .transpose()?,
                    interpolation: from_optional(fields[5])
                        .map(|word| parse(interpolation_from, &word, at, "interpolation"))
                        .transpose()?,
                    location: None,
                };
                declared_type(&mut reflection, fields[1]).fields.push(field);
            }
            "count" => {
                count(4)?;
                let target = parse(BackendTarget::parse, fields[2], at, "backend")?;
                let binding = parse_number(fields[3], at)?;
                let owner = reflection
                    .resources
                    .iter_mut()
                    .find(|resource| resource.resource_name == fields[1])
                    .ok_or_else(|| ReflectionError::Unanchored {
                        line: at,
                        name: fields[1].to_owned(),
                    })?;
                owner.length_bindings.push((target, binding));
            }
            "layout" => {
                count(4)?;
                let layout = ReflectedLayout {
                    class: "uniform".to_owned(),
                    alignment: parse_number(fields[2], at)?,
                    size: parse_number(fields[3], at)?,
                    fields: Vec::new(),
                };
                declared_type(&mut reflection, fields[1]).uniform_layout = Some(layout);
            }
            "field" => {
                count(7)?;
                let laid_out = ReflectedLayoutField {
                    name: fields[2].to_owned(),
                    offset: parse_number(fields[3], at)?,
                    alignment: parse_number(fields[4], at)?,
                    size: parse_number(fields[5], at)?,
                    stride: parse_number(fields[6], at)?,
                };
                let owner = reflection
                    .types
                    .iter_mut()
                    .find(|declared| declared.name == fields[1])
                    .and_then(|declared| declared.uniform_layout.as_mut())
                    .ok_or_else(|| ReflectionError::Unanchored {
                        line: at,
                        name: fields[1].to_owned(),
                    })?;
                owner.fields.push(laid_out);
            }
            other => {
                return Err(ReflectionError::UnknownRecord {
                    line: at,
                    keyword: other.to_owned(),
                });
            }
        }
    }
    if !seen_shader {
        return Err(ReflectionError::Unanchored {
            line: 1,
            name: "shader".to_owned(),
        });
    }
    Ok(reflection)
}

/// The reflected type named `name`, opening one when no record has yet.
///
/// `member` and `layout` both describe a type and either may come first, so
/// neither opens it exclusively.
fn declared_type<'a>(reflection: &'a mut Reflection, name: &str) -> &'a mut ReflectedType {
    // Indexing rather than `last_mut` keeps this total: the position is either
    // found or is the one just pushed, so there is nothing to unwrap.
    let at = match reflection
        .types
        .iter()
        .position(|declared| declared.name == name)
    {
        Some(at) => at,
        None => {
            reflection.types.push(ReflectedType {
                name: name.to_owned(),
                fields: Vec::new(),
                uniform_layout: None,
                storage_layout: None,
            });
            reflection.types.len() - 1
        }
    };
    &mut reflection.types[at]
}

/// The stage the last `stage` record opened, for a record that extends it.
fn last_stage(
    reflection: &mut Reflection,
    at: usize,
) -> Result<&mut ReflectedStage, ReflectionError> {
    reflection
        .stages
        .last_mut()
        .ok_or_else(|| ReflectionError::Unanchored {
            line: at,
            name: "stage".to_owned(),
        })
}

/// Applies `parser` to `value`, turning a rejection into a typed error.
fn parse<T>(
    parser: impl Fn(&str) -> Option<T>,
    value: &str,
    at: usize,
    expected: &'static str,
) -> Result<T, ReflectionError> {
    parser(value).ok_or_else(|| ReflectionError::BadField {
        line: at,
        value: value.to_owned(),
        expected,
    })
}

/// Reads a `u32`, turning a rejection into a typed error.
fn parse_number(value: &str, at: usize) -> Result<u32, ReflectionError> {
    value.parse().map_err(|_| ReflectionError::BadField {
        line: at,
        value: value.to_owned(),
        expected: "number",
    })
}

/// Reads the comma-separated stage list a resource is visible in.
fn parse_visibility(value: &str, at: usize) -> Result<Vec<Stage>, ReflectionError> {
    if value == "-" {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|word| parse(stage_from, word, at, "stage"))
        .collect()
}

/// `-` for an absent optional field.
fn optional(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

/// The value an optional field holds, when it holds one.
fn from_optional(value: &str) -> Option<String> {
    if value == "-" {
        None
    } else {
        Some(value.to_owned())
    }
}

/// How a resource's visibility is written.
fn visibility(stages: &[Stage]) -> String {
    if stages.is_empty() {
        return "-".to_owned();
    }
    stages
        .iter()
        .map(|&stage| stage_word(stage))
        .collect::<Vec<_>>()
        .join(",")
}

macro_rules! words {
    ($to:ident, $from:ident, $ty:ty, $($word:literal => $variant:path),+ $(,)?) => {
        /// How the value is written in a `KSLR1` document.
        fn $to(value: $ty) -> &'static str {
            match value { $($variant => $word),+ }
        }
        /// The value `word` names, when it names one.
        fn $from(word: &str) -> Option<$ty> {
            Some(match word { $($word => $variant,)+ _ => return None })
        }
    };
}

words!(stage_word, stage_from, Stage,
    "vertex" => Stage::Vertex,
    "fragment" => Stage::Fragment,
    "compute" => Stage::Compute,
);

words!(shader_kind_word, shader_kind, ShaderKind,
    "graphics" => ShaderKind::Graphics,
    "compute" => ShaderKind::Compute,
);

words!(group_class_word, group_class_from, GroupClass,
    "frame" => GroupClass::Frame,
    "pass" => GroupClass::Pass,
    "material" => GroupClass::Material,
    "object" => GroupClass::Object,
    "draw" => GroupClass::Draw,
    "dispatch" => GroupClass::Dispatch,
    "custom" => GroupClass::Custom,
);

words!(resource_kind_word, resource_kind_from, ResourceKind,
    "uniform" => ResourceKind::Uniform,
    "storage" => ResourceKind::Storage,
    "texture" => ResourceKind::Texture,
    "sampler" => ResourceKind::Sampler,
);

// Appended rather than inserted: this table is a text round-trip a reflection
// string is written with, so a word already emitted keeps its meaning and an
// older reader simply does not know this one.
words!(access_word, access_from, AccessMode,
    "read" => AccessMode::Read,
    "read_write" => AccessMode::ReadWrite,
    "write" => AccessMode::Write,
);

words!(interpolation_word, interpolation_from, Interpolation,
    "perspective" => Interpolation::Perspective,
    "linear" => Interpolation::Linear,
    "flat" => Interpolation::Flat,
);

words!(builtin_word, builtin_from, Builtin,
    "position" => Builtin::Position,
    "vertex_index" => Builtin::VertexIndex,
    "instance_index" => Builtin::InstanceIndex,
    "front_facing" => Builtin::FrontFacing,
    "frag_coord" => Builtin::FragCoord,
    "thread_id" => Builtin::ThreadId,
    "local_id" => Builtin::LocalId,
    "group_id" => Builtin::GroupId,
    "local_index" => Builtin::LocalIndex,
);

/// Renders every resource in the compact digest a graphics host parses.
///
/// Not a second reflection format competing with [`encode`] — a different
/// contract. `KSLR1` is the whole reflection, versioned and round-trippable,
/// for anything that wants to read what a shader declares. This is the string
/// a graphics runtime parses when it configures a pipeline, and its shape is
/// that consumer's, not ours: one record per resource, records separated by
/// `;`, each opening with a letter naming its kind.
///
/// ```text
/// u|name:binding:size:stageMask:memberCount:member,member:kinds
/// s|name:binding:stageMask:glslBinding:readonly
/// t|name:binding:stageMask:samplerBinding:glslName
/// i|name:binding:stageMask:glslBinding:writeonly
/// m|name:binding:stageMask
/// ```
///
/// `binding` is the **WGSL** binding throughout — which is also the slot an
/// application binds against — and `stageMask` is bit 0 vertex, bit 1 fragment,
/// bit 2 compute.
///
/// A uniform (`u`) carries its `std140` size and its members, each written
/// `name@offset#size`. A member's `size` is its *natural* size, not its padded
/// one: the host maps that size onto a GL uniform type, and a 3-wide vector has
/// to arrive as 12 so it maps to `FLOAT3` rather than `FLOAT4`. Offsets stay
/// `std140`, which is where the value actually sits. `kinds` is one letter per
/// member in the same order — `f` float, `i` signed integer, `u` unsigned —
/// because a size alone cannot tell `float` from `int`, and a GL host loads the
/// two through different calls.
///
/// A storage buffer (`s`) carries the GLSL `binding` its `std430` block was
/// emitted with and whether it is read-only. A texture (`t`) carries the public
/// slot of the sampler its body samples it with — `255` when no stage samples it
/// — and the name it takes in GLSL, where the two collapse into one `sampler2D`.
/// A texture the shader **writes** is a storage image (`i`) instead: it is not a
/// `sampler2D` on any backend, it is bound as an image rather than a texture,
/// and it carries the GLSL image unit its `layout(binding = n)` was emitted with
/// plus whether the shader only ever writes it. A sampler (`m`) carries only
/// where it binds.
///
/// The resource name comes last in the records that carry one, so a reader can
/// split the fixed fields on `:` without a name having to avoid the separator.
#[must_use]
pub fn resource_digest(reflection: &Reflection) -> String {
    let mut out = String::new();
    for resource in &reflection.resources {
        let binding = target_binding(resource, BackendTarget::Wgsl);
        let mut mask = 0u32;
        for stage in &resource.visibility {
            mask |= match stage {
                Stage::Vertex => 1,
                Stage::Fragment => 2,
                Stage::Compute => 4,
            };
        }
        match resource.resource_kind {
            ResourceKind::Uniform => {
                let Some(declared) = reflection
                    .types
                    .iter()
                    .find(|declared| declared.name == resource.type_name)
                else {
                    continue;
                };
                let Some(layout) = &declared.uniform_layout else {
                    continue;
                };
                out.push_str(&format!(
                    "u|{}:{binding}:{}:{mask}:{}",
                    resource.resource_name,
                    layout.size,
                    layout.fields.len()
                ));
                let mut kinds = String::new();
                for (at, field) in layout.fields.iter().enumerate() {
                    out.push(if at == 0 { ':' } else { ',' });
                    let member = declared
                        .fields
                        .iter()
                        .find(|member| member.name == field.name);
                    let natural =
                        member.map_or(field.size, |member| natural_size(&member.type_name));
                    kinds.push(member.map_or('f', |member| scalar_kind(&member.type_name)));
                    out.push_str(&format!("{}@{}#{natural}", field.name, field.offset));
                }
                if !kinds.is_empty() {
                    out.push(':');
                    out.push_str(&kinds);
                }
            }
            ResourceKind::Storage => {
                let readonly = u32::from(resource.access != Some(AccessMode::ReadWrite));
                out.push_str(&format!(
                    "s|{}:{binding}:{mask}:{}:{readonly}",
                    resource.resource_name,
                    target_binding(resource, BackendTarget::Glsl430)
                ));
            }
            // A written texture is a storage image, and nothing about it is
            // shaped like a sampled one: no sampler pairs with it, and it binds
            // at an image unit rather than a texture unit.
            ResourceKind::Texture
                if matches!(
                    resource.access,
                    Some(AccessMode::Write | AccessMode::ReadWrite)
                ) =>
            {
                let writeonly = u32::from(resource.access == Some(AccessMode::Write));
                out.push_str(&format!(
                    "i|{}:{binding}:{mask}:{}:{writeonly}",
                    resource.resource_name,
                    target_binding(resource, BackendTarget::Glsl430)
                ));
            }
            ResourceKind::Texture => {
                let sampler = resource
                    .paired_sampler
                    .as_ref()
                    .and_then(|name| {
                        reflection
                            .resources
                            .iter()
                            .find(|other| &other.resource_name == name)
                    })
                    .map_or(255, |other| target_binding(other, BackendTarget::Wgsl));
                let glsl_name = resource
                    .backend_bindings
                    .iter()
                    .find(|entry| entry.target == BackendTarget::Glsl430)
                    .and_then(|entry| entry.glsl_name.clone())
                    .unwrap_or_else(|| resource.resource_name.clone());
                out.push_str(&format!(
                    "t|{}:{binding}:{mask}:{sampler}:{glsl_name}",
                    resource.resource_name
                ));
            }
            ResourceKind::Sampler => {
                out.push_str(&format!("m|{}:{binding}:{mask}", resource.resource_name));
            }
        }
        out.push(';');
    }
    out
}

/// The binding `resource` takes on `target`, or 0 if it has none.
fn target_binding(resource: &ReflectedResource, target: BackendTarget) -> u32 {
    resource
        .backend_bindings
        .iter()
        .find(|binding| binding.target == target)
        .map_or(0, |binding| binding.binding_index)
}

/// The unpadded size of a member type, as the host's type table expects it.
/// The letter naming a uniform member's scalar kind in the digest.
///
/// A member's byte size says how wide it is and nothing about how it is loaded:
/// `float` and `int` are both four bytes and a GL host reaches them through
/// `glUniform1fv` and `glUniform1iv`, which are not interchangeable. Matrices
/// are float by construction, and anything unrecognized is float because that is
/// what every non-integer KSL type is.
fn scalar_kind(type_name: &str) -> char {
    match type_name {
        "Int" | "Int2" | "Int3" | "Int4" | "Bool" => 'i',
        "UInt" | "UInt2" | "UInt3" | "UInt4" => 'u',
        _ => 'f',
    }
}

fn natural_size(type_name: &str) -> u32 {
    match type_name {
        "Float" | "Int" | "UInt" | "Bool" => 4,
        "Float2" | "Int2" | "UInt2" => 8,
        "Float3" | "Int3" | "UInt3" => 12,
        "Float4" | "Int4" | "UInt4" => 16,
        "Float2x2" => 32,
        "Float3x3" => 48,
        "Float4x4" => 64,
        _ => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Reflection {
        Reflection {
            shader_name: "Tri".to_owned(),
            shader_kind: ShaderKind::Graphics,
            backend: BackendTarget::Msl,
            options: vec![ReflectedOption {
                name: "use_tint".to_owned(),
                type_name: "Bool".to_owned(),
                default_value: "true".to_owned(),
            }],
            stages: vec![ReflectedStage {
                stage: Stage::Vertex,
                entry_name: "vertex_main".to_owned(),
                input_type: Some("VertexIn".to_owned()),
                output_type: Some("VertexOut".to_owned()),
                threads: Some([64, 1, 1]),
                inputs: vec![ReflectedField {
                    name: "position".to_owned(),
                    type_name: "Float2".to_owned(),
                    builtin: None,
                    interpolation: None,
                    location: Some(0),
                }],
                outputs: vec![ReflectedField {
                    name: "clip_position".to_owned(),
                    type_name: "Float4".to_owned(),
                    builtin: Some(Builtin::Position),
                    interpolation: Some(Interpolation::Flat),
                    location: None,
                }],
            }],
            types: vec![ReflectedType {
                name: "Camera".to_owned(),
                fields: Vec::new(),
                uniform_layout: Some(ReflectedLayout {
                    class: "uniform".to_owned(),
                    alignment: 16,
                    size: 64,
                    fields: vec![ReflectedLayoutField {
                        name: "view_projection".to_owned(),
                        offset: 0,
                        alignment: 16,
                        size: 64,
                        stride: 64,
                    }],
                }),
                storage_layout: None,
            }],
            resources: vec![ReflectedResource {
                group_name: "Frame".to_owned(),
                group_class: GroupClass::Frame,
                group_index: 0,
                resource_name: "camera".to_owned(),
                resource_kind: ResourceKind::Uniform,
                type_name: "Camera".to_owned(),
                visibility: vec![Stage::Vertex, Stage::Fragment],
                access: Some(AccessMode::Read),
                backend_bindings: vec![BackendBinding {
                    target: BackendTarget::Msl,
                    group_index: 0,
                    binding_index: 1,
                    glsl_name: None,
                }],
                length_bindings: vec![(BackendTarget::Msl, 3)],
                paired_sampler: Some("linear".to_owned()),
            }],
        }
    }

    #[test]
    fn a_document_round_trips_field_for_field() {
        let original = sample();
        let decoded = decode(&encode(&original)).expect("decodes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn the_magic_opens_the_document() {
        assert!(encode(&sample()).starts_with("KSLR1\n"));
        assert!(matches!(
            decode("NOPE1\nshader S graphics msl\n"),
            Err(ReflectionError::BadMagic { .. })
        ));
        assert!(matches!(decode(""), Err(ReflectionError::BadMagic { .. })));
    }

    #[test]
    fn an_unknown_record_is_refused_rather_than_skipped() {
        // A host that ignored a binding record it did not understand would draw
        // with the wrong resources instead of failing.
        let text = format!("{MAGIC}\nshader S graphics msl\nmystery 1 2 3\n");
        assert!(matches!(
            decode(&text),
            Err(ReflectionError::UnknownRecord { .. })
        ));
    }

    #[test]
    fn every_truncation_is_an_error_and_none_is_a_panic() {
        let whole = encode(&sample());
        for cut in 0..whole.len() {
            let truncated = &whole[..cut];
            // Only the assertion that it does not panic matters here; a prefix
            // may legitimately still decode.
            let _ = decode(truncated);
        }
    }

    #[test]
    fn a_record_with_the_wrong_field_count_names_what_it_wanted() {
        let text = format!("{MAGIC}\nshader S graphics\n");
        assert!(matches!(
            decode(&text),
            Err(ReflectionError::FieldCount { wanted: 4, .. })
        ));
    }

    #[test]
    fn a_binding_before_its_resource_is_refused() {
        let text = format!("{MAGIC}\nshader S graphics msl\nbind ghost msl 0 1 -\n");
        assert!(matches!(
            decode(&text),
            Err(ReflectionError::Unanchored { .. })
        ));
    }

    #[test]
    fn a_document_with_no_shader_record_is_refused() {
        let text = format!("{MAGIC}\n");
        assert!(matches!(
            decode(&text),
            Err(ReflectionError::Unanchored { .. })
        ));
    }

    #[test]
    fn an_unparseable_number_is_a_typed_error() {
        let text = format!(
            "{MAGIC}\nshader S graphics msl\nstage vertex entry e input - output -\nthreads x 1 1\n"
        );
        assert!(matches!(
            decode(&text),
            Err(ReflectionError::BadField {
                expected: "number",
                ..
            })
        ));
    }
}
