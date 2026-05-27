# Platforms

Kira models runners with typed ids, not ad hoc strings:

- `desktop`
- `macos`
- `ios`
- `tvos`
- `visionos`
- `windows`
- `android`
- `web`
- `linux`

Every runner id is valid for command parsing. If the current host or generated runner client cannot complete the flow, Kira reports a precise diagnostic such as missing Xcode tools, missing Visual Studio tools, missing Android SDK/Gradle, or a modeled-but-incomplete runner client.

The resolved default config is synthesized even when TOML omits it:

```toml
[profiles.debug]
backend = "vm"
optimization = "none"
debug_symbols = true

[profiles.profiler]
backend = "llvm"
optimization = "speed-lite"
debug_symbols = true
profiling = true

[profiles.release]
backend = "llvm"
optimization = "speed"
debug_symbols = false
strip = true
lto = true
```

Default runners are also synthesized: desktop uses `kira`, Apple platforms use `xcode`, Windows uses `visual-studio`, Android uses `android-studio`, Web uses `kira-wasm` with `default_surface = "dom"`, and Linux uses `cmake`.

Web surface requirements are also typed. `dom` is DOM rendering, while `webgpu` is a graphics-canvas surface that requires a canvas and browser WebGPU capability detection. This is platform modeling for Kira Graphics and is separate from Kira Web DOM rendering.

TOML remains mostly optional. A minimal app can be:

```toml
[package]
name = "example"
kind = "app"
```

`profiles.profiler` is the supported profiling profile name. `profiles.profile` is reserved so configs do not silently drift between noun and mode names.

Target inference is command-aware. `kira live ios` means runner `ios` and the current project target. `kira live ./ios` means the default desktop runner and target path `./ios`.

iOS live reporting keeps generic iPhoneOS build, simulator fallback, physical-device detection, provisioning, install, launch, and live protocol states separate. When a physical iPhone is unavailable, Kira audits the simulator fallback instead of claiming physical-device success.

Android live reporting keeps SDK/Gradle availability, physical-device detection, emulator detection, install, launch, and live protocol states separate. Emulator-only success is not physical Android success.
