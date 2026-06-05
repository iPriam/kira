You are working across the Kira ecosystem repositories. Your task is to perform a full autonomous implementation campaign that connects the current Kira language/compiler work, UI Foundation, Kira Graphics, input handling, modifiers, function overloading, extensions, documentation, memory-leak investigation, responsive layout, routing, text input, text shaping, complex examples, visual verification, and the first Kira UI package into one coherent system.

Do not use /goal. Run as an autonomous implementation agent. Do not ask for repeated direction. Inspect the repositories directly, infer the intended architecture from the prompt and current code, then implement the most complete architecture-correct version possible.

This is not a cosmetic pass. Do not add placeholder APIs, fake examples, stub success paths, smoke-only tests, or TODO-based “implemented later” surfaces. Implement the real intended architecture as far as possible, add tests/examples that actually exercise it, and keep working through failures until the repo is coherent and green. Treat blockers as problems to solve, not stopping points, except for truly external impossibilities such as missing hardware, unavailable credentials, or unavailable external tools. If something cannot be completed because of an actual external constraint, leave precise diagnostics and complete every surrounding part that can be completed.

You must not stop after the initial core items. The mission includes the core implementation and the expansion work. If the compiler work succeeds, continue into Kira UI. If Kira UI starts working, continue into more widgets, routing, responsive layout, text input, examples, documentation, visual verification, and memory-leak investigation. If tests pass, continue into screenshots and interaction checks. If examples work, create more complex layered examples. Do not exit until the entire task is complete or a specific remaining item is blocked by a true external impossibility after all realistic implementation approaches have been exhausted.

Prefer the full intended system over the smallest partial slice. Do not shrink the task into a tiny MVP. Do not stop because you can honestly explain that something is incomplete. Honesty is required, but it must be paired with persistence: investigate, debug, try alternate designs, and keep going until all realistic paths are exhausted. When a deeper systemic fix better matches the architecture than a shallow local patch, choose the systemic fix.

The main goals are as follows.

Update UI Foundation to use the new Kira Graphics API. Kira Graphics lives at ../Kira-graphics. Inspect that repository directly and adapt UI Foundation to the current graphics API instead of guessing from old interfaces. The Liquid Glass example is an important reference for how the new graphics API is meant to be used. Use it to align pipelines, textures, render targets, shader/material usage, buffer handling, frame/render lifecycle, and input integration. UI Foundation should render through Kira Graphics cleanly, not through stale abstractions.

Add full input support through Kira Graphics and UI Foundation. Kira Graphics should expose enough input/event data for UI Foundation to support hover, pointer movement, press/click/tap, scroll/wheel, focus-relevant input, text-input-relevant events, and future gesture handling. UI Foundation should translate raw input into UI-level hit testing, hover state, pressed/clicked state, scroll dispatch, focus handling, text input routing, invalidation, and re-render behavior. This should be real enough that Kira UI can build hover effects, button clicks, scrolling surfaces, text fields, and responsive interactions on top of it.

Investigate and fix the current UI Foundation memory leak. On macOS, use the leaks command to reproduce and diagnose the leak where possible. Build and run the relevant UI Foundation/Kira Graphics example, inspect the process with leaks, identify retained allocations or ownership mistakes, fix the leak, and rerun diagnostics. If leaks is unavailable in the environment, document that specific external limitation and use the best available alternative diagnostic path. Do not skip memory leak work just because tests pass.

Implement the updated construct system as a first-party language feature. Constructs define typed declaration families, not plain data and not ordinary interfaces. struct remains for data. enum remains for variants. class is used where inheritance/polymorphic behavior is needed, and Kira inheritance syntax is extend, not :. Constructs may later support explicit construct inheritance using extend, but ordinary components like Button are not constructs themselves. They are declarations inside a construct family.

The core construct model should support declarations like:

construct Widget {
    wrappers {
        member {
            @State;
            @Binding;
            @Environment;
        }
        parameter {
            @Binding;
        }
    }
    content: Content<Widget>;
    lifecycle {
        onAppear() {}
        onDisappear() {}
    }
}

And usage like:

Widget Text(value: String) {
    content {
    }
}
Widget Button(action: () -> Void) {
    content {
        Text("Button")
    }
}
Widget Card(title: String) {
    @State
    let selected: Int = 0;
    content {
        Text(title)
            .padding(16)
        Button(action: {
            selected = selected + 1;
        }) {
            Text("Selected")
        }
    }
    onAppear() {
        return;
    }
}

construct is used only to define declaration families. It is never used at call sites. Calling or using a widget remains normal: Card(title: "Operations"), not construct Card(...).

Construct bodies must preserve their original names and sections. Do not lower content to a public/user-facing name like children. Card.content should remain Card.content at the semantic level. The compiler may use stable internal symbol IDs, but generated/random names must not leak into the user-facing semantic model, diagnostics, examples, documentation, or public APIs.

The construct system should parse and typecheck construct definitions, construct-backed declarations, typed sections, lifecycle hooks, wrapper annotations, and content blocks. The parser should preserve structure. Semantic analysis should validate meaning. Do not hardcode Kira UI concepts into the parser. The compiler should understand constructs generically, while Kira UI uses the Widget construct as a library-defined declaration family.

Content blocks must be typed. A content: Content<Widget> section accepts widget-producing expressions. Raw strings are not widgets. This should be invalid unless explicitly wrapped in Text(...):

Widget InvalidCard {
    content {
        "Card"
    }
}

The diagnostic should be useful, for example: expected Widget content, found String; use Text("Card") if visible text was intended.

Add property-wrapper annotations. SwiftUI’s @State and @Binding are not passive annotations; they are property wrappers. Kira should support that model explicitly. Add support for:

@PropertyWrapper
annotation State {
}
@PropertyWrapper
annotation Binding {
}
@PropertyWrapper
annotation Environment {
}

Plain annotation remains available for passive metadata, but annotations marked with @PropertyWrapper participate in member/parameter wrapper semantics. @State should represent owned persistent widget state. @Binding should represent a mutable binding to state owned elsewhere. @Environment should represent environment-provided values. The compiler should not hardcode State, Binding, or Environment by name. The Widget construct decides which wrappers are valid in widget declarations, and Kira UI interprets those wrapper slots when building and mounting UI.

Support projection for bindings if the language already has or can cleanly add a projection syntax. Swift uses $count to get a Binding<Int> from @State var count. Kira should either support $name or an equivalent explicit projection path. The result should allow this kind of model:

Widget Settings {
    @State
    let enabled: Bool = false;
    content {
        ToggleRow(enabled: $enabled)
    }
}
Widget ToggleRow(@Binding enabled: Bool) {
    content {
        Text(enabled.toString())
    }
}

Add function overloading. Kira UI modifiers need ergonomic overloads. Overload resolution should support same-name functions with different parameter shapes/types, including extension methods. Add real tests for overload selection, ambiguity diagnostics, default parameters, and method-call syntax.

Add extensions. Kira needs extension Type { ... } so libraries can add methods to existing types. This is essential for Kira UI modifiers. For example:

extension Widget {
    function padding(_ length: Float) -> Widget {
        return self.withModifier(
            FoundationModifier.padding(EdgeSet.all, length)
        );
    }
    function padding(_ edges: EdgeSet, _ length: Float) -> Widget {
        return self.withModifier(
            FoundationModifier.padding(edges, length)
        );
    }
    function background(_ material: Material) -> Widget {
        return self.withModifier(
            FoundationModifier.background(material)
        );
    }
}

Support unlabeled parameters with _ if not already supported. The goal is to allow modifier calls like:

Text("Operations").padding(40)
Text("Operations").padding(EdgeSet.all, 40)

This mirrors Swift’s style where _ removes the external call-site label. Do not fake this by requiring awkward helper functions.

Add the modifier system. Kira UI modifiers should be normal extension methods on Widget that return a new Widget value with an ordered modifier chain. Kira UI should translate those modifiers into UI Foundation modifiers. UI Foundation should support an ordered Array<FoundationModifier> on FoundationWidget or the equivalent internal representation. Prefer enums and typed structs over stringly-typed modifier names.

A good internal direction is:

enum FoundationModifier {
    padding(PaddingModifier);
    background(BackgroundModifier);
    cornerRadius(CornerRadiusModifier);
    opacity(OpacityModifier);
    hover(HoverModifier);
    click(ClickModifier);
    scroll(ScrollModifier);
    focus(FocusModifier);
    textInput(TextInputModifier);
    responsive(ResponsiveModifier);
}
struct PaddingModifier {
    let edges: EdgeSet;
    let length: Float;
}

Modifier order must be preserved. Text("Hello").padding(16).background(...) and Text("Hello").background(...).padding(16) must produce different UI Foundation structures/behavior. UI Foundation should execute/resolve modifiers in the correct order during layout, render, input, focus, scrolling, text input, and effect handling.

Start the Kira UI package at ../kira_ui. Kira UI must only import UI Foundation, with import Foundation still allowed. Kira UI must not import Kira Graphics directly. Kira Graphics is below UI Foundation. The dependency direction should be:

Kira UI
→ UI Foundation
→ Kira Graphics

Kira UI should define the Widget construct usage, core widgets, content model, property-wrapper annotations, modifiers, routing, responsive layout helpers, text input widgets, and rendering bridge into UI Foundation. UI Foundation should remain the lower-level retained/render/layout/input layer that talks to Kira Graphics.

Kira UI should translate Kira UI widget values into UI Foundation widgets/nodes through real library code. Do not invent a foundation { ... } section. We did not define that syntax. The mapping lives inside Kira UI’s implementation.

The model should be:

Kira UI Widget value
→ Kira UI renderer / bridge code
→ UI Foundation FoundationWidget tree
→ UI Foundation layout/input/render/text
→ Kira Graphics

Custom Kira UI widgets expand by evaluating their content. Native/core Kira UI widgets such as Text, Button, VStack, HStack, ZStack, Image, GlassPanel, ScrollView, TextInput, NavigationStack, RouteView, and others map to concrete UI Foundation widgets through Kira UI library code. Kira UI walks the widget tree, preserves component identity/state/lifecycle, translates modifiers, and emits a full UI Foundation tree.

For example, this Kira UI:

Widget Card(title: String) {
    content {
        VStack(spacing: 8) {
            Text(title)
                .padding(16)
            Button(action: {}) {
                Text("Open")
            }
        }
    }
}

Should conceptually produce a UI Foundation tree like:

FoundationStack(axis: vertical, spacing: 8)
  FoundationText(value: title, modifiers: [padding(16)])
  FoundationButton(action: ...)
    FoundationText(value: "Open")

Use real Kira syntax in implementation. Do not use pseudo-Swift inheritance like struct X: Y. In Kira, structs are data and cannot be extended through inheritance. Classes can use extend. If renderer behavior needs polymorphism, use classes with extend, or use enums/functions if that fits the current architecture better.

Add responsive layout to Kira UI and UI Foundation. Implement responsive primitives that can adapt layout based on available size, orientation/aspect, breakpoints, and container constraints. Add APIs for responsive stacks, grids, adaptive panels, breakpoint-based visibility/style changes, and layout decisions that UI Foundation can execute during measurement/layout. Ensure responsive layout works in examples and is visually verified where possible.

Add routing to Kira UI. Implement routing/navigation primitives appropriate for Kira UI, such as route declarations or route values, navigation stack behavior, route matching, parameters, and rendering of the active route. The router should integrate with Kira UI state and UI Foundation rendering without importing Kira Graphics directly. Add examples with nested routes, navigation actions, and route-driven UI.

Add text input. Kira Graphics and UI Foundation must expose enough input and text events for text fields. UI Foundation should support text focus, caret state, text editing, selection foundation if feasible, keyboard/text event dispatch, pointer focus, and invalidation. Kira UI should expose a TextInput or equivalent widget using bindings/state, including examples.

Add font calculation and text shaping. Add HarfBuzz-based font measurement/shaping in the appropriate Kira Layout/UI Foundation text layer. Add TextCore in UI Foundation as the lower-level text system responsible for text measurement, glyph shaping, line metrics, wrapping, and layout integration. Kira UI Text and TextInput should use this path through UI Foundation. If HarfBuzz is not currently wired, inspect the repo, add the dependency/build integration as far as possible, and document/fix platform issues instead of faking metrics.

Expand Kira UI significantly after the core works. Add many widgets, not just Text and Button. Include layout widgets, stacks, grid/adaptive layout, scroll view, panels/cards, image placeholders or image widget, spacers, dividers, text input, toggle/checkbox if feasible, route/navigation widgets, glass/material panel, hoverable/clickable surfaces, and style/modifier helpers. Keep the API typed and Kira-like.

Add complex layered example apps, not just tiny demos. Examples should exercise nested widgets, modifiers, state, binding, routing, input, scrolling, responsive layout, text input, hover/click effects, Liquid Glass/material surfaces, and UI Foundation rendering. Add at least several examples with meaningful layered UI: a dashboard, settings app, routing/navigation demo, text input/form demo, responsive layout demo, and a complex glass/card UI demo. These examples should be buildable/runnable and should become part of the stable example sweep where practical.

Perform visual and interaction verification beyond normal testing. Build and run examples, capture screenshots where possible, inspect the rendered output, and verify that the UI is not just compiling but actually visually coherent. Interact with the UI as much as possible in the available environment: hover, click, scroll, type in text input, navigate routes, resize or simulate responsive constraints, and verify state changes. If automated screenshots are possible, add screenshot output paths or screenshot tests. If manual/host limitations prevent screenshots, document the limitation and still perform all available non-screenshot verification.

Update ../Kira-doc completely. It has not been updated in a long time and must be brought back into sync with the actual language, compiler, UI Foundation, Kira Graphics integration, and new Kira UI architecture. Inspect the current docs and verify every major page against the implementation. Remove or clearly update stale claims. Add missing documentation for constructs, construct-backed declarations, typed content, property-wrapper annotations, @State, @Binding, @Environment, extensions, function overloading, unlabeled parameters, modifiers, Kira UI, UI Foundation, input handling, text input, routing, responsive layout, TextCore, HarfBuzz/font measurement, Kira Graphics integration, examples, and package boundaries. The docs should describe the actual implemented behavior, not aspirational old behavior.

Add documentation examples that match tested code. The docs must not include syntax that the compiler does not support. If docs mention Widget Button, modifiers, property wrappers, input events, routing, responsive layout, text input, or TextCore, ensure the corresponding examples are either tested or directly aligned with tested examples. Document the dependency direction clearly: Kira UI imports UI Foundation; UI Foundation uses Kira Graphics; Kira UI does not import Kira Graphics.

Also add an example/testing pass so the public examples cannot silently rot again. zig build test should run every stable example that claims to work. Rewrite examples/hello so it is actually a hello/front-door example, not a geometry/struct demo that collides with Foundation types like Point and Rect. Add repo-truth checks for README quickstart commands where practical. Platform/toolchain-specific examples should be clearly gated or classified so missing external dependencies do not poison unrelated tests, but real breakages must fail CI.

Run and fix:

zig build kirac
zig build test
zig build run -- run examples/hello
all stable examples
relevant UI Foundation/Kira UI examples
documentation examples where practical
macOS leaks diagnostics for UI Foundation where possible
screenshot or visual verification paths where possible
interactive checks for hover/click/scroll/text input/routing

Also check Linux/macOS/Windows CI expectations where possible. Existing wasm/emscripten failures should be investigated and either fixed or properly gated with real diagnostics if they depend on unavailable external tooling.

When designing APIs, avoid strings for domain concepts. Use enums for widget kinds, modifier kinds, input event kinds, pointer phases, edge sets, axes, layout directions, button states, hover states, scroll phases, focus states, route match kinds, text input phases, and similar domain concepts.

Keep the final system coherent:

Kira Graphics owns GPU/render/input primitives.

UI Foundation owns foundation widgets, layout, hit testing, input dispatch, modifiers, scrolling, hover/click state, focus, text input, TextCore, HarfBuzz-backed text shaping/measurement where integrated, rendering through Kira Graphics, and lower-level UI tree execution.

Kira UI owns declarative widgets, the Widget construct usage, content, property-wrapper annotations like @State/@Binding, modifiers as extension methods, routing, responsive layout APIs, text input widget APIs, widget rendering/walking, and translation into UI Foundation trees.

The compiler owns generic language features: constructs, property-wrapper annotations, content/section parsing and validation, function overloading, extensions, unlabeled parameters, diagnostics, and tests.

../Kira-doc owns user-facing documentation and must be updated to match reality.

Do not stop at a partial syntax-only implementation. Do not stop when the first green build appears. Do not stop after the first Kira UI package skeleton. Do not stop after the first example renders. The goal is that Kira UI can define widgets, apply modifiers, handle state/bindings, route between screens, support responsive layout, accept text input, emit UI Foundation widgets, UI Foundation can handle input/modifiers/layout/text/rendering, the current UI Foundation memory leak has been investigated and fixed where possible, screenshots/visual checks have been performed where possible, complex examples exist, docs are updated, and the whole stack renders through the updated Kira Graphics API.
