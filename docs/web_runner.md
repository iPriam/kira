# Kira Wasm Web Runner

The `web` runner id maps to Kira Wasm, the web runner/runtime backend. Kira Wasm is not Kira Web.

- Foundation provides low-level platform APIs, including `Foundation.Web`.
- Kira Wasm is the runner/runtime backend for browsers.
- Kira Web is reserved for a future React-alternative framework.
- Kira UI/UI Foundation/Kira Graphics are separate from Kira Web.

Web surfaces are typed:

- `dom`
- `webgpu`
- `hybrid`

The current executable proofs are `dom` and `webgpu`. Web exports and web live runs write a generated Kira Wasm runtime module, load it through `WebAssembly.instantiate`, and expose the runtime status to browser glue. `webgpu` additionally creates a canvas, requests a WebGPU adapter/device, builds a WGSL triangle pipeline, submits one render pass, and records browser-side smoke state. `hybrid` is still rejected with a precise diagnostic until it has a browser VM/native boundary runner.

Foundation.Web browser APIs are FFI-backed. The generated binding surface includes DOM node handles, console logging, navigator/location access, attributes/styles/classes, stable callback registration/invocation/removal, DOM event hooks, timers, callback cleanup, and WebGPU capability detection. Generated JS glue lives in web exports as `kira-browser-ffi.generated.js`.

`examples/web_dom` demonstrates:

- creating DOM elements
- setting text
- appending children
- logging `Kira browser API call succeeded`
- showing location/user-agent data
- updating text to `Kira DOM updated`

Commands:

```bash
kira check examples/web_dom
kira run web examples/web_dom --quit-after 1s
kira live web examples/web_dom --surface dom --quit-after 10s
kira export web examples/web_dom --surface dom
kira export web examples/web_dom --surface webgpu
```

The generated `kira-app.wasm` artifact is a real Wasm module with exported runtime probes. Kira no longer emits an 8-byte header-only placeholder and the web runner does not require `emcc` for this generated-runtime path.
