# Commands

Standalone CLI:

- `zig build install`
- `zig build install-kirac`
- `kira --help`
- `kira --version`
- `kira fetch-llvm`
- `kira fetch-llvm --ci-metadata --json`
- `kira fetch-llvm --archive /path/to/llvm.tar.xz`
- `kira run examples/hello`
- `kira run examples/hello --quit-after 5s`
- `kira run --backend llvm examples/hello`
- `kira run --backend hybrid examples/hybrid_roundtrip`
- `kira live examples/hello --quit-after 5s`
- `kira live desktop examples/hello --quit-after 5s`
- `kira live desktop examples/hello --run-for 5s --kill-after`
- `kira live ios-simulator examples/hello --quit-after 5s`
- `kira tokens examples/hello`
- `kira ast examples/hello`
- `kira check examples/hello`
- `kira build examples/hello`
- `kira shader check examples/shaders/textured_quad.ksl`
- `kira shader ast examples/shaders/textured_quad.ksl`
- `kira shader build examples/shaders/textured_quad.ksl`
- `kira shader build`
- `kira instruments run examples/arithmetic --backend runtime --track memory --track cpu --duration 500ms --sample-rate 10hz --fail-on-growth 1gb`
- `kira sync`
- `kira add FrostUI`
- `kira add --git https://github.com/Sunlight-Horizon/GameKit.git --rev <commit> GameKit`
- `kira remove FrostUI`
- `kira update`
- `kira package pack`
- `kira package inspect generated/DemoApp-0.1.0.tar`
- `kira new --lib GraphicsKit generated/GraphicsKit`
- `kira build --backend llvm examples/hello`
- `kira build --backend hybrid examples/hybrid_roundtrip`
- `kira new DemoApp generated/DemoApp`

Build-system convenience:

- `zig build`
- `zig build kirac`
- `zig build kira-bootstrapper`
- `zig build test`
- `zig build fetch-llvm`
- `zig build run -- run examples/hello`
- `zig build run -- run examples/hello --quit-after 5s`
- `zig build run -- live examples/hello --quit-after 5s`
- `zig build run -- run --backend llvm examples/hello`
- `zig build run -- run --backend hybrid examples/hybrid_roundtrip`
- `zig build run -- tokens examples/hello`
- `zig build run -- ast examples/hello`
- `zig build run -- check examples/hello`
- `zig build run -- build examples/hello`
- `zig build run -- shader check examples/shaders/textured_quad.ksl`
- `zig build run -- shader build examples/shaders/textured_quad.ksl`
- `zig build run -- shader build`
- `zig build run -- instruments run examples/arithmetic --backend runtime --track memory --track cpu --duration 500ms --sample-rate 10hz --json-out .kira/instruments/arithmetic.runtime.json`
- `zig build run -- build --backend llvm examples/hello`
- `zig build run -- build --backend hybrid examples/hybrid_roundtrip`
- `zig build run -- new DemoApp generated/DemoApp`

Install notes:

- `zig build install` installs `kira` into `zig-out/bin/` by default and installs the active real toolchain into `~/.kira/toolchains/<channel>/<version>/`
- `zig build install-kirac` installs the same managed toolchain plus launcher flow without changing the rest of the repo install names
- `zig build install -p .local` installs into `.local/bin/` instead of `zig-out/bin/`
- `~/.kira/toolchains/current.toml` selects which real toolchain `kira` forwards to
- GitHub release archives ship the `kira` launcher separately from the managed toolchain payload; on first run, the release launcher downloads the matching `kira-toolchain-<platform>` archive into `~/.kira/toolchains/release/<version>/` and activates it automatically
- add the chosen launcher `bin/` directory to `PATH` to make direct `kira` invocation global for your shell session

CLI behavior:

- `fetch-llvm` reads `llvm-metadata.toml`, resolves the current host bundle, downloads the matching GitHub release asset, installs it into `~/.kira/toolchains/llvm/<llvm-version>/<target>/`, and skips when the install marker already matches
- `fetch-llvm --ci-metadata --json` prints Kira-owned machine-readable metadata for CI without downloading or extracting anything
- `fetch-llvm --archive <path>` installs a previously downloaded archive into the managed LLVM location using the same validation, extraction, marker, and layout rules as the normal fetch flow
- `run`, `build`, `check`, `tokens`, and `ast` default to the current directory and discover `kira.toml` first, then legacy `project.toml`
- `run` defaults to the VM backend; `run --backend llvm` builds and runs a native executable
- `run --backend hybrid` builds a hybrid manifest, bytecode sidecar, and native shared library, then runs the mixed program in the hybrid host
- `run <target> --quit-after <duration>` starts the target and requests or enforces bounded shutdown after a positive duration such as `5s`, `5000ms`, or plain integer seconds. This is intended for non-disruptive smoke tests of graphical and long-running examples.
- `live <target> --quit-after <duration>` defaults to the desktop platform and starts a real live server/client session. The server builds a VM/live-loadable bundle graph under the selected target's `.kira-build/live/`, launches a live runner client, sends the bundle graph and bundle payloads, waits for client load/link/entrypoint events, waits for at least one rendered frame unless `--headless` is explicitly supplied, and then requests clean shutdown when the duration expires.
- `live desktop <target> --quit-after <duration>` is the explicit desktop spelling. The desktop runner window is visible for renderable apps/examples; `--quit-after` only bounds the session and does not bypass server/client handshake, bundle delivery, entrypoint start, or frame acknowledgement.
- `live desktop <target> --run-for <duration> --kill-after` is a legacy compatibility spelling. `--run-for` maps to the same bounded duration as `--quit-after`; `--kill-after` is retained as an emergency cleanup hint after graceful shutdown has been attempted.
- `live ios-simulator <target>` and `live ios <target>` are recognized platform spellings. The current implementation audits Xcode, the iPhoneSimulator SDK, and available simulator runtimes, then reports a precise unsupported-runner diagnostic until the simulator live client build/install/launch path is implemented. Physical device live support is recognized as `ios-device` but is not implemented.
- Live root handling is explicit: invocation cwd, selected target root, Kira toolchain root, runner host path, generated live output root, and client runtime cwd are separate. The desktop runner host is resolved from the installed/development Kira toolchain; the selected example directory is never used as a Zig build root.
- Live reload currently uses full-bundle hot restart. On source changes, the server rebuilds the bundle graph, sends the full bundle set, and the already-running client restarts the app entrypoint without rebuilding or relaunching the runner process. Incremental bundle patching is reserved for a future protocol extension and is not documented as supported yet.
- `--headless` is only for non-window tests of the live protocol and reload loop. Normal desktop live sessions open a visible window when the platform can present one.
- `run`, `build`, and `check` automatically sync dependencies before compiling; add `--offline` or `--locked` when you want cache-only or lockfile-only behavior
- Library package roots are checkable and buildable, but not runnable or live-runnable. `kira run .` on a library emits `KCL020`; `kira live .` emits `KCL021`. Use an example or app target for execution.
- Examples and app packages are the runnable surface for CLI smoke tests. The real sibling-project matrix runs examples with `--quit-after` so graphical windows and live runners do not remain open.
- VM, LLVM/native, and hybrid now share the ordinary executable surface for control flow, calls, arrays, named values, and mixed runtime/native interaction
- `tokens` dumps lexer output
- `ast` dumps the parsed AST
- `check` runs parse and semantics
- `build` defaults to writing a `.kbc` bytecode artifact into `generated/`
- `shader check` runs the dedicated `.ksl` lexer, parser, import loader, semantic pass, and typed shader IR validation
- `shader ast` dumps the parsed KSL module shape without routing through the executable `.kira` frontend
- `shader build <file.ksl>` emits GLSL 330 vertex/fragment source plus reflection JSON into `generated/shaders/` next to the source file by default, or `--out-dir <dir>`
- `shader build` with no explicit file discovers all top-level PascalCase `*.ksl` entry shaders under `Shaders/` in the current project root and writes outputs to `generated/Shaders/`
- `shader build` rejects compute shaders today with an explicit backend diagnostic because the current real graphics path in this repo is Sokol/OpenGL with GLSL 330 graphics shaders, not a compute-capable pipeline
- `instruments run <target>` builds the target through the normal pipeline, launches the selected backend as a child process, samples process metrics over the requested duration, prints a stable human report, and optionally writes stable JSON with `--json-out`
- `instruments run` accepts `--backend runtime|llvm|hybrid`, repeated `--track memory` and `--track cpu`, `--duration` values like `30s`, `1m`, or `500ms`, `--sample-rate` values like `10hz` or `2.5hz`, and `--fail-on-growth` byte thresholds like `10mb`, `512kb`, or `1048576`
- `instruments run --track memory` samples the child process private working set on Windows, which is the Task Manager-like resident private memory metric; JSON keeps the stable `rss_*` field names and includes `"metric": "private_working_set"` to document the measured source. On other platforms memory instrumentation fails clearly until platform samplers are added
- `instruments run --track cpu` reports measured CPU percent when enough samples are available, and reports CPU data as unavailable rather than inventing values when the run is too short or the platform data is not available
- `instruments run --fail-on-growth <bytes>` exits non-zero when measured memory growth is greater than the threshold; equality is treated as passing
- `sync` resolves registry, path, and git dependencies into `kira.lock`, verifies registry archive SHA-256 checksums, and populates the local cache under `~/.kira/cache/packages/`
- `add`, `remove`, and `update` edit `kira.toml` and then refresh `kira.lock`
- `package pack` writes a validated source-only `.tar` archive into `generated/`
- `package inspect` prints manifest metadata and archive contents without extracting package scripts because package scripts are not supported
- `build --backend llvm` writes both a native object file and a native executable into `generated/`
- `build --backend hybrid` writes a `.khm` hybrid manifest plus the bytecode, native object, and native shared library sidecars into `generated/`
- `new` scaffolds either an app or a library package; use `new --lib` for a library template with `kind = "library"`, `module_root`, and a root module file under `app/`

LLVM backend selection is explicit and host-native. Discovery order is:

1. `KIRA_LLVM_HOME`
2. `~/.kira/toolchains/llvm/<llvm-version>/<target>/`
3. older repo-managed fallback paths if they already exist

The pinned LLVM fetch flow intentionally does not use checksum verification.
