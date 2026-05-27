# Architecture

This bootstrap uses a strict layered package graph. Higher layers may depend on lower layers, and lower layers never depend upward.

The shared compiler pipeline is:

1. `kira_source` loads source text and spans
2. `kira_lexer` tokenizes
3. `kira_parser` builds AST
4. `kira_program_graph` resolves imports and builds the package-rooted source graph from canonical `app/` source roots only
5. `kira_semantics` validates exactly one `@Main` function, resolves locals, and lowers to HIR
6. `kira_ir` lowers HIR into backend-facing IR
7. backend selection happens in `kira_build`

Project roots remain metadata roots for manifests, lockfiles, native-library configuration, generated asset metadata, and build/install state. They are not Kira source roots. Source graph construction only admits files from the target package's `app/` directory and the `app/` directories of declared dependency packages.

The VM backend path is:

1. `kira_bytecode` compiles shared IR into bytecode
2. `kira_vm_runtime` executes bytecode

The LLVM-native backend path is:

1. `kira_llvm_backend` lowers shared IR through the text-LLVM path, verifies it, and emits a real object file for ordinary language execution
2. `kira_native_bridge` provides the stable native helper symbols used by LLVM lowering for builtin printing
3. `kira_build` links the emitted object and helper object into a host-native executable through Zig's linker driver

The hybrid path is:

1. `@Runtime` functions compile to bytecode and stay under `kira_vm_runtime`
2. `@Native` functions compile to native code and are linked into a shared library
3. `kira_hybrid_runtime` loads both artifacts in one process
4. native-to-runtime calls go through an installed native bridge callback
5. runtime-to-native calls go through native trampolines resolved from the shared library

`@Runtime` and `@Native` are execution-boundary annotations, not usability restrictions. Outside direct FFI usage, runtime code and native code can call each other naturally, pass values in both directions, and use the same shared executable surface through the bridge/trampoline layer.

`kira_build_definition` and `kira_backend_api` stay backend-neutral. `kira_cli` stays a leaf command surface. `kira_main` remains the app-facing C ABI facade rather than becoming compiler glue.

Hybrid packages still remain separate, but the repo is no longer structurally VM-only. Shared IR, runtime ABI direction, and stable native helper calls keep future hybrid work additive instead of architectural repair work.

`kira_main` is intentionally separate from compiler packages. It is the app-facing C ABI facade that generated apps will link against.

## KSL Pipeline

`.ksl` should remain a sibling pipeline rather than being forced through the executable `.kira` frontend and IR path.

The implemented shape is:

1. `kira_source` loads `.ksl` source text and spans
2. `kira_ksl_parser` tokenizes and parses shader AST
3. `kira_ksl_semantics` resolves groups, resources, stage IO, options, and layouts
4. `kira_shader_ir` preserves typed shader meaning for diagnostics, reflection, and lowering
5. `kira_glsl_backend` lowers graphics shaders to GLSL 330 plus reflection
6. `kira_wgsl_backend` lowers graphics shaders to WGSL plus reflection for WebGPU-class targets
7. `kira_build` and `kira_cli` expose `kira shader check|ast|build`

Shader targets are typed by `kira_shader_model.BackendTarget`. GLSL 330 and WGSL currently have validated lowering paths; HLSL, MSL, and SPIR-V are recognized as target identities and rejected before artifact writes until backend-specific lowering and runtime validation are present.

This keeps Kira's main executable language and KSL's shader language coherent without pretending they are the same thing. See [docs/ksl.md](ksl.md).
