# KSL v1 Design

## Status

This document now describes the implemented first serious version of `.ksl`, the Kira Shading Language.

It is intentionally designed as a dedicated sibling language, not as "normal Kira with shader annotations" and not as a syntax skin over GLSL, HLSL, or WGSL.

The current repo now includes:

- `kira_ksl_syntax_model`
- `kira_ksl_parser`
- `kira_ksl_semantics`
- `kira_shader_ir`
- `kira_shader_model`
- `kira_glsl_backend`
- `kira_wgsl_backend`
- `kira_hlsl_backend`
- `kira_msl_backend`
- `kira_spirv_backend`
- `kira_build` and `kira_cli` integration via `kira shader check|ast|build`

The concrete emitted graphics backends are GLSL 330, WGSL, HLSL, MSL, and textual SPIR-V assembly. GLSL 330 remains the current Kira Graphics/Sokol desktop path; WGSL is emitted and validated for WebGPU-class shader module and render-pipeline creation; MSL is syntax-checked through Apple's Metal compiler when available.

Compute shaders are part of the language surface, parser, semantic model, and reflection path, but `kira shader build` intentionally rejects them for now on the validated graphics backends with a clear diagnostic instead of pretending they work on the current graphics stack.

## Design Laws

`.ksl` inherits Kira's language laws:

- readable first
- inferred unless ambiguous
- no visible angle-bracket generics in user syntax
- ambiguity is always a compile error
- semantic meaning stays in the language; backend numbering stays out of normal source

For shaders, those laws lead to four concrete constraints:

1. `.ksl` is semantic-first.
2. `.ksl` is source-level backend-neutral.
3. `.ksl` is strict about numeric and resource ambiguity.
4. `.ksl` keeps the core small enough to lower cleanly to real GPU APIs.

## v1 Boundary

### In Scope

- vertex shaders
- fragment shaders
- compute shaders
- shared types and helper functions
- named resource groups
- uniform data
- storage buffers
- sampled textures
- samplers
- stage input and output types
- built-in stage semantics
- interpolation control
- compile-time options
- reflection for host integration
- early portability diagnostics

### Out of Scope

- ray tracing
- mesh or task shaders
- user-facing pointers
- backend-specific source branching
- visible descriptor-set or binding syntax in ordinary source
- visible Metal argument indices in ordinary source
- bindless resource models
- trait systems or advanced generics
- storage images in v1
- geometry or tessellation stages

## Chosen Shape

The canonical unit is a `shader` declaration.

That is the right fit for Kira because:

- it groups resources, options, and entry stages into one reflection unit
- it matches the way real graphics and compute pipelines are created
- it avoids free-floating `@vertex` and `@fragment` functions with unclear ownership
- it keeps shared module items separate from pipeline-local declarations

`.ksl` files may still contain top-level `import`, KSL-specific `type`, and `function` declarations. Those declarations are shared shader helpers; Kira application source uses canonical `class` and `struct` instead. `shader` blocks are the actual compile targets.

## Source Model

The top-level source shape is:

```ksl
import Common.Lighting as Lighting

type CameraUniform {
    let view_projection: Float4x4
}

function saturate(value: Float) -> Float {
    if value < 0.0 { return 0.0 }
    if value > 1.0 { return 1.0 }
    return value
}

shader LitSurface {
    option use_vertex_color: Bool = false

    group Frame {
        uniform camera: CameraUniform
    }

    vertex {
        input VertexIn
        output VertexToFragment

        function entry(input: VertexIn) -> VertexToFragment {
            ...
        }
    }

    fragment {
        input VertexToFragment
        output FragmentOut

        function entry(input: VertexToFragment) -> FragmentOut {
            ...
        }
    }
}
```

## Type System

### Scalar Types

KSL v1 scalar types are fixed and explicit:

- `Bool`
- `Int`
- `UInt`
- `Float`

For v1, these mean:

- `Int` is a 32-bit signed integer
- `UInt` is a 32-bit unsigned integer
- `Float` is a 32-bit float

There is no abstract-width integer model in `.ksl`. Shader code must lower predictably.

### Vector And Matrix Types

KSL uses readable built-in names instead of generic syntax:

- `Float2`, `Float3`, `Float4`
- `Int2`, `Int3`, `Int4`
- `UInt2`, `UInt3`, `UInt4`
- `Float2x2`, `Float3x3`, `Float4x4`
- mixed-shape matrices such as `Float3x4` are legal when the backend set supports them

### Resource Types

Sampled and bound resource types are named directly:

- `Texture2d`
- `TextureCube`
- `DepthTexture2d`
- `Sampler`
- `ComparisonSampler`

Storage buffers are declared through resource declarations, not generic wrapper types.

### User Types

User-defined `type` declarations are allowed for:

- uniforms
- stage IO structs
- helper-only value types
- storage buffer element types

Library aliases such as `Color`, `Transform`, or `Vertex` are allowed, but remain library-level. The compiler only hardcodes the minimal shader core.

### Numeric Inference

KSL is intentionally strict.

- floating literals resolve to `Float`
- integer literals have no standalone default type
- an integer literal must be constrained by context or an explicit declaration

This is valid:

```ksl
let cascade_count: UInt = 4
```

This is invalid:

```ksl
let cascade_count = 4
```

Reason: `4` could become `Int` or `UInt`, and KSL does not silently choose.

### Coercions

Implicit coercions are intentionally minimal:

- no implicit `Int` to `UInt`
- no implicit `UInt` to `Int`
- no implicit integer-to-float promotion
- no implicit scalar-to-vector splat
- no implicit matrix multiply shape correction

Explicit conversion uses a type constructor:

```ksl
let count_as_float = Float(light_count)
let tint = Float4(1.0, 0.8, 0.6, 1.0)
```

Matrix multiplication is always explicit with `mul(a, b)`. KSL does not overload `*` for matrix math in v1 because row/column-major assumptions become portability traps.

## Resource Model

### Resource Groups

Resources live inside named `group` blocks:

```ksl
group Frame {
    uniform camera: CameraUniform
    uniform lighting: SceneLighting
}

group Material {
    uniform surface: SurfaceParams
    texture albedo: Texture2d
    sampler linear: Sampler
}
```

The intended common group names are:

- `Frame`
- `Pass`
- `Material`
- `Object`
- `Draw`
- `Dispatch`

Custom names are legal, but those canonical names are what Kira Graphics should treat as first-class conventions.

### Resource Declarations

KSL v1 supports these forms:

```ksl
uniform camera: CameraUniform
storage read particles: [Particle]
storage read_write counters: [UInt]
texture albedo: Texture2d
texture shadow_map: DepthTexture2d
sampler linear: Sampler
sampler shadow_compare: ComparisonSampler
```

Rules:

- `uniform` values are read-only
- `storage read` is read-only
- `storage read_write` is writable
- textures are read-only in v1
- samplers are separate from textures in source

KSL keeps textures and samplers separate because:

- Vulkan/WGSL/Metal all have different pairing and binding models
- source-level pairing syntax would either leak backend details or hide real constraints badly
- `sample(texture, sampler, uv)` is explicit and portable

### Access Rules

- writing to `uniform` data is always illegal
- writing through `storage read` is always illegal
- `storage read_write` is legal only in compute in v1
- a `ComparisonSampler` may only be used with depth textures and compare sampling intrinsics

### Backend Mapping

Source never contains descriptor set or binding numbers.

The compiler assigns backend bindings deterministically:

- SPIR-V and WGSL: each KSL `group` becomes one descriptor set or bind group
- group order is assigned by canonical class order first, then declaration order for custom groups
- resources inside a group receive stable binding numbers in declaration order
- Metal: the compiler keeps group semantics in reflection and lowers to argument buffers by default

Reflection always reports the final backend mapping so host code never needs handwritten slot tables.

## Stage Model

### Stage Blocks

KSL v1 supports three stage sections inside a shader:

- `vertex`
- `fragment`
- `compute`

Graphics shaders use `vertex` and optionally `fragment`.
Compute shaders use `compute`.
A single `shader` declaration may be either graphics or compute, but not both.

### Entry Shape

Each stage block declares:

- an input type
- an output type when the stage has one
- exactly one `entry` function

Example:

```ksl
vertex {
    input VertexIn
    output VertexToFragment

    function entry(input: VertexIn) -> VertexToFragment {
        ...
    }
}
```

```ksl
compute {
    threads(64, 1, 1)
    input ComputeIn

    function entry(input: ComputeIn) {
        ...
    }
}
```

### Stage IO

Stage IO uses ordinary `type` declarations with field annotations for built-ins and interpolation.

Example:

```ksl
type VertexIn {
    let position: Float3
    let normal: Float3
    let uv: Float2

    @builtin(vertex_index)
    let vertex_index: UInt

    @builtin(instance_index)
    let instance_index: UInt
}

type VertexToFragment {
    @builtin(position)
    let clip_position: Float4

    let world_normal: Float3
    let uv: Float2

    @interpolate(flat)
    let material_index: UInt
}

type FragmentOut {
    let color: Float4
}
```

### Built-In Semantics

v1 built-ins are:

- `position`
- `vertex_index`
- `instance_index`
- `front_facing`
- `frag_coord`
- `thread_id`
- `local_id`
- `group_id`
- `local_index`

The semantic checker validates stage legality:

- `position` is required on vertex output
- `position` is also allowed on fragment input so a shared vertex-output/fragment-input struct can carry clip-space position through the current graphics path
- `vertex_index` and `instance_index` are vertex-input-only built-ins
- `front_facing` and `frag_coord` are fragment-input-only built-ins
- `thread_id`, `local_id`, `group_id`, and `local_index` are compute-input-only built-ins

### Interpolation

Interpolation is controlled per field:

- default: perspective-correct
- `@interpolate(linear)`
- `@interpolate(flat)`

Rules:

- integer varyings require `flat`
- fragment inputs must match the vertex output field set by name and type
- interpolation mismatches are compile errors

### Compute Threads

Compute shaders declare workgroup size explicitly:

```ksl
compute {
    threads(8, 8, 1)
    ...
}
```

`threads(...)` arguments must be compile-time constants. They may be integer literals with explicit context or `option` values.

KSL v1 enforces a portable floor:

- `x * y * z <= 256`

That matches the conservative guaranteed limit expected by WebGPU-class targets and prevents attractive but non-portable defaults.

## Compile-Time Options

Options are pipeline-specialization values:

```ksl
shader Tonemap {
    option apply_gamma: Bool = true
    option workgroup_width: UInt = 64
    ...
}
```

Rules:

- options are compile-time constants
- option defaults must be literal or constant-foldable expressions
- option values are reflected to the host API
- hosts may override option values at shader compilation or pipeline creation time

Options are intentionally not a preprocessor.

This is allowed:

```ksl
if apply_gamma {
    color = pow(color, Float3(1.0 / 2.2))
}
```

This is not part of v1:

- conditionally creating or removing resources
- target-specific conditional branches in source

## Surface Examples

### Shared Helper Module

```ksl
type DirectionalLight {
    let direction: Float3
    let intensity: Float
    let color: Float3
}

type SceneLighting {
    let ambient: Float3
    let sun: DirectionalLight
}

function saturate(value: Float) -> Float {
    if value < 0.0 { return 0.0 }
    if value > 1.0 { return 1.0 }
    return value
}

function lambert(normal: Float3, light_direction: Float3) -> Float {
    let n = normalize(normal)
    let l = normalize(-light_direction)
    return saturate(dot(n, l))
}
```

### Simple Lit Surface Shader

```ksl
import Common.Lighting as Lighting

type CameraUniform {
    let view_projection: Float4x4
}

type TransformUniform {
    let model: Float4x4
}

type SurfaceUniform {
    let albedo_color: Float3
    let alpha: Float
}

type VertexIn {
    let position: Float3
    let normal: Float3
}

type VertexToFragment {
    @builtin(position)
    let clip_position: Float4

    let world_normal: Float3
}

type FragmentOut {
    let color: Float4
}

shader LitSurface {
    group Frame {
        uniform camera: CameraUniform
        uniform lighting: Lighting.SceneLighting
    }

    group Object {
        uniform transform: TransformUniform
    }

    group Material {
        uniform surface: SurfaceUniform
    }

    vertex {
        input VertexIn
        output VertexToFragment

        function entry(input: VertexIn) -> VertexToFragment {
            let out: VertexToFragment
            let world_position = mul(transform.model, Float4(input.position, 1.0))
            out.clip_position = mul(camera.view_projection, world_position)
            out.world_normal = normalize(input.normal)
            return out
        }
    }

    fragment {
        input VertexToFragment
        output FragmentOut

        function entry(input: VertexToFragment) -> FragmentOut {
            let out: FragmentOut
            let diffuse = Lighting.lambert(input.world_normal, lighting.sun.direction) * lighting.sun.intensity
            let lit = lighting.ambient + (lighting.sun.color * diffuse)
            out.color = Float4(surface.albedo_color * lit, surface.alpha)
            return out
        }
    }
}
```

### Textured Shader

```ksl
type CameraUniform {
    let view_projection: Float4x4
}

type SurfaceUniform {
    let tint: Float4
}

type VertexIn {
    let position: Float3
    let uv: Float2
}

type VertexToFragment {
    @builtin(position)
    let clip_position: Float4

    let uv: Float2
}

type FragmentOut {
    let color: Float4
}

shader TexturedQuad {
    option use_tint: Bool = true

    group Frame {
        uniform camera: CameraUniform
    }

    group Material {
        uniform surface: SurfaceUniform
        texture albedo: Texture2d
        sampler linear: Sampler
    }

    vertex {
        input VertexIn
        output VertexToFragment

        function entry(input: VertexIn) -> VertexToFragment {
            let out: VertexToFragment
            out.clip_position = mul(camera.view_projection, Float4(input.position, 1.0))
            out.uv = input.uv
            return out
        }
    }

    fragment {
        input VertexToFragment
        output FragmentOut

        function entry(input: VertexToFragment) -> FragmentOut {
            let out: FragmentOut
            let sampled = sample(albedo, linear, input.uv)
            if use_tint {
                out.color = sampled * surface.tint
            } else {
                out.color = sampled
            }
            return out
        }
    }
}
```

### Compute Shader

```ksl
type SimulationUniform {
    let delta_time: Float
    let damping: Float
}

type Particle {
    let position: Float3
    let velocity: Float3
}

type ComputeIn {
    @builtin(thread_id)
    let thread_id: UInt3

    @builtin(local_id)
    let local_id: UInt3

    @builtin(group_id)
    let group_id: UInt3
}

shader ParticleIntegrate {
    option workgroup_width: UInt = 64

    group Dispatch {
        uniform simulation: SimulationUniform
        storage read_write particles: [Particle]
    }

    compute {
        threads(workgroup_width, 1, 1)
        input ComputeIn

        function entry(input: ComputeIn) {
            let index: UInt = input.thread_id.x
            if index >= particles.count {
                return
            }

            let particle = particles[index]
            let next: Particle
            next.position = particle.position + (particle.velocity * simulation.delta_time)
            next.velocity = particle.velocity * simulation.damping
            particles[index] = next
            return
        }
    }
}
```

## Validation Rules

KSL v1 should reject invalid code early and precisely.

### Stage Rules

- graphics shaders must declare exactly one vertex entry
- compute shaders must declare exactly one compute entry
- a compute shader may not also declare vertex or fragment stages
- a fragment stage may not exist without a vertex stage in v1
- vertex output and fragment input must match by field name, type, and interpolation

### Resource Rules

- resource names must be unique inside a shader
- group names must be unique inside a shader
- writable storage resources are compute-only in v1
- textures and samplers are distinct resource declarations
- depth textures may only be used with depth sampling intrinsics

### Type Rules

- integer literals without constraining type are errors
- integer varyings require `@interpolate(flat)`
- resource structs may not contain textures or samplers
- `Bool` may not be used as a storage-buffer element type in v1
- runtime-sized arrays are only legal in storage-buffer declarations

### Portability Rules

- `threads(x, y, z)` must stay within the portable limit
- unsupported resource kinds are rejected before backend lowering
- unsupported built-ins for a selected stage are rejected before backend lowering
- matrix multiply order must be explicit through `mul`

## Diagnostics

Representative diagnostics:

### Ambiguous Numeric Literal

```text
error[KSL021]: ambiguous integer literal
KSL could not infer whether `4` should be `Int` or `UInt`.
--> particles_update.ksl:12:31
help: write `let workgroup_width: UInt = 4` or convert explicitly
```

### Invalid Stage IO

```text
error[KSL041]: fragment input does not match vertex output
The field `world_normal` is `Float2` here, but the vertex stage outputs `Float3`.
--> lit_surface.ksl:48:9
help: make the fragment input field match the vertex output exactly
```

### Mismatched Resource Usage

```text
error[KSL062]: unknown shader resource
The fragment stage uses `Material.normal_map`, but no resource with that name exists in group `Material`.
--> textured_quad.ksl:37:26
help: declare the resource in the shader's `group Material { ... }` block or remove the access
```

### Invalid Writable Access

```text
error[KSL071]: resource is not writable
`Frame.camera` is a uniform resource and cannot be assigned to.
--> lit_surface.ksl:29:13
help: write to a local value or move writable state into a `storage read_write` resource
```

### Missing Stage Entry

```text
error[KSL081]: missing vertex entry
Graphics shader `LitSurface` declares fragment work but no vertex `entry`.
--> lit_surface.ksl:20:1
help: add a `vertex { ... function entry(...) ... }` block
```

### Unsupported Feature

```text
error[KSL091]: pointers are not supported in KSL v1
KSL does not expose user-facing pointers.
--> broken.ksl:4:15
help: express the data through uniforms, storage buffers, or value types instead
```

### Early Portability Failure

```text
error[KSL101]: workgroup size exceeds the portable limit
`threads(32, 16, 1)` creates 512 invocations, but KSL v1 guarantees portability only up to 256.
--> blur.ksl:18:5
help: reduce the workgroup size or split the work across more groups
```

## Reflection

Reflection is a required compiler output.

The host must be able to discover:

- shader name
- shader kind: graphics or compute
- entry stage names
- option names, types, defaults, and overrideability
- groups and resources
- per-resource access mode
- per-resource stage visibility
- stage IO fields and built-in semantics
- final backend binding assignments
- uniform and storage layout metadata

Recommended v1 sidecar format:

- human-readable JSON for tooling and debugging
- optional compact binary form later

Example sketch:

```json
{
  "shader": "LitSurface",
  "kind": "graphics",
  "options": [
    { "name": "use_vertex_color", "type": "Bool", "default": false }
  ],
  "groups": [
    {
      "name": "Frame",
      "class": "frame",
      "backend": { "wgsl_group": 0, "spirv_set": 0, "metal_argument_buffer": 0 },
      "resources": [
        {
          "name": "camera",
          "kind": "uniform",
          "type": "CameraUniform",
          "visibility": ["vertex"]
        }
      ]
    }
  ]
}
```

The important rule is that host code binds by semantic names and reflection data, not by handwritten binding tables.

## Layout And Portability Model

KSL must preserve logical layout until backend lowering.

v1 uses two compiler-defined layout classes:

- `uniform` layout
- `storage` layout

The compiler computes:

- byte offsets
- alignment
- array stride
- matrix stride
- total struct size

Those values are emitted in reflection. Host integration uses reflection instead of assuming C layout.

This avoids source-level `std140`, `std430`, `packed`, or backend-private attributes in ordinary `.ksl`.

## Compiler Architecture

### Why Not Reuse The Current Parser Directly

The current compiler is built around a single `.kira` language surface with executable HIR and a required `@Main` path.

That is the wrong abstraction boundary for shaders because:

- shaders are not executable programs with a single `@Main`
- graphics shaders are pipeline assets, not normal functions
- stage IO, resource groups, and reflection are first-class semantic objects
- forcing them through the current executable IR would either leak backend details or warp the language

### Chosen Compiler Placement

`.ksl` should live inside the repo, but as a sibling shader pipeline that shares lower-level infrastructure.

Keep:

- `kira_source`
- `kira_diagnostics`
- general build orchestration patterns

Separate:

- KSL syntax
- KSL semantic analysis
- KSL typed IR
- shader backends

### Package Plan

Recommended package layout:

- Layer 1: `kira_ksl_syntax_model`
- Layer 1: `kira_ksl_parser`
- Layer 2: `kira_shader_model`
- Layer 2: `kira_ksl_semantics`
- Layer 3: `kira_shader_ir`
- Layer 4: backend lowerers such as `kira_glsl_backend`, `kira_wgsl_backend`, `kira_hlsl_backend`, `kira_msl_backend`, and `kira_spirv_backend`
- Layer 6: `kira_build` integration and CLI dispatch

This change adds `kira_shader_model` as the first concrete step.

### Parsing Model

KSL gets a dedicated lexer/parser pair.

Reason:

- the syntax is related to Kira, but not identical
- shader-only keywords such as `shader`, `group`, `uniform`, `storage`, `texture`, `sampler`, `vertex`, `fragment`, `compute`, `option`, `input`, `output`, and `threads` should not pollute the main Kira grammar prematurely
- it keeps `.kira` stability high while KSL iterates

### AST Shape

The KSL AST should model:

- imports
- helper types
- helper functions
- shader declarations
- options
- resource groups and resources
- stage blocks
- interface types
- annotations for built-ins and interpolation

### Semantic Analysis

The semantic checker should:

- resolve names across imports, types, functions, and shader-local declarations
- validate stage legality
- validate resource access and visibility
- infer integer literals only when context removes ambiguity
- compute compile-time option values
- compute uniform and storage layout metadata
- assign stable logical resource identities
- derive stage visibility from actual use
- produce reflection data directly from semantic results

### Typed Shader IR

The typed shader IR must preserve:

- shader kind
- groups and resources
- access modes
- stage IO fields and semantics
- compile-time options
- constant-folded values
- logical layouts
- backend mapping decisions as a later pass, not source syntax

This IR is not the same as the current executable IR and should not be forced through `kira_ir`.

### Backend Lowering

Backends should lower from typed shader IR:

- GLSL 330 text path for the current repo graphics flow
- WGSL text path for WebGPU
- HLSL path for D3D-style targets
- SPIR-V path for Vulkan
- MSL text path for Metal

Backend lowerers must own:

- binding-number assignment
- backend-specific attribute spelling
- MSL argument-buffer generation
- target-specific restrictions that were not already rejected by the portability layer

### Reflection Generation

Reflection should come from the typed semantic model before backend lowering finalizes codegen details, then be augmented with final backend assignments after lowering.

### Hot Reload

Hot reload should compare two hashes:

- implementation hash: function bodies and constants
- interface hash: options, groups, resources, layouts, and stage IO

If only implementation changes, Kira Graphics may rebuild pipelines without rebinding semantics.
If the interface hash changes, the host must invalidate cached pipeline layouts and rebinding tables.

### Kira Graphics Integration

Today `KiraGraphics` stores raw shader strings:

- `Scene.vertex_source`
- `Scene.fragment_source`

That should evolve to shader assets built from `.ksl`.

The future host flow should be:

1. compile `.ksl` to per-target backend artifacts plus reflection
2. load reflection as the canonical host-side shader schema
3. create graphics or compute pipelines from shader assets, not raw strings
4. bind resources by group and resource name

That keeps Kira Graphics semantic-first instead of turning it into a binding-number API.

## Implementation Status

The current repo now has:

1. the existing `.kira` pipeline unchanged
2. `kira_shader_model` as the shared shader vocabulary
3. `kira_ksl_syntax_model` and `kira_ksl_parser`
4. `kira_ksl_semantics` with diagnostics and layout computation
5. `kira_shader_ir`
6. `kira shader check <file.ksl>`, `kira shader ast <file.ksl>`, and `kira shader build [<file.ksl>]`
7. reflection emission as JSON sidecar output
8. concrete graphics backends: `kira_glsl_backend`, `kira_wgsl_backend`, `kira_hlsl_backend`, `kira_msl_backend`, and `kira_spirv_backend`

Still deferred:

1. compute-capable backend lowering
2. binary SPIR-V assembly/validation when a local `spirv-as`/`spirv-val` toolchain is available
3. project-level shader asset integration beyond the direct CLI workflow
4. reflection-driven runtime graphics pipeline consumption

## Test Plan

Implementation is not complete until these tests exist.

### Unit Tests

- group-classification rules
- built-in legality per stage and direction
- interpolation validation
- numeric literal ambiguity detection
- layout computation for uniforms and storage buffers
- reflection emission shape

### Parser Tests

- valid shader blocks
- valid group declarations
- valid options
- stage blocks with `input`, `output`, and `threads`
- malformed resource declarations
- malformed built-in annotations

### Semantic Pass Tests

- graphics shader with valid vertex and fragment stages
- compute shader with valid writable storage buffer
- missing required vertex `position` output
- fragment input and vertex output mismatch
- integer varying without `flat`
- write to uniform
- write to `storage read`
- ambiguous integer literal
- illegal use of unsupported types or pointers
- non-portable workgroup size

### Shader Corpus

Recommended new test layout:

- `tests/shaders/pass/graphics/...`
- `tests/shaders/pass/compute/...`
- `tests/shaders/fail/parser/...`
- `tests/shaders/fail/semantics/...`
- `tests/shaders/reflect/...`

Each corpus case should contain:

- `main.ksl`
- `expect.toml`
- optional reflection snapshot

### Golden Backend Tests

Current:

- GLSL 330 golden output checks
- WGSL backend package checks
- WGSL CLI artifact checks
- real Chrome WebGPU shader-module, render-pipeline, and render-pass validation for generated WGSL
- HLSL, MSL, and SPIR-V CLI artifact checks
- MSL compiler checks with `xcrun metal`
- reflection JSON checks
- explicit compute-backend rejection checks
- CLI shader command checks

Later:

- WGSL golden output tests
- binary SPIR-V validation tests
- MSL golden output tests

## Tradeoffs

The main deliberate tradeoffs in v1 are:

- no attempt to make shaders look like ordinary executable Kira code
- no backend numbers in ordinary source
- no implicit integer typing
- no writeable graphics-stage storage in v1
- no storage images in v1
- no matrix operator overloading in v1

Those restrictions are intentional. They keep the language elegant where it matters and explicit where GPU portability would otherwise become guesswork.
