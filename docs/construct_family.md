# Construct Declaration Families

`construct` defines a *declaration family*: a typed template that other declarations conform to.
It is not data (`struct`), not variants (`enum`), and not inheritance-of-behavior (`class`).
This page documents the construct-family language surface validated by the compiler. Every form
below is exercised by the corpus under `tests/pass/check/construct_*`,
`tests/pass/run/attempt_try_handle`, and `tests/fail/semantics/construct_*` / `tests/fail/semantics/attempt_*`.

This layer is **validation-complete** (parse + semantic checks + diagnostics). Construct-backed
declarations themselves do not execute yet; `attempt`/`try`/`handle` does execute (see below).

## Inheritance: `extends`

Constructs inherit from other constructs with `extends` (not `:` and not `extend`). Multiple
parents are allowed; the local inheritance graph must be acyclic.

```kira
construct WebElement {}
construct Drawable {}
construct Surface extends WebElement, Drawable {}
```

Diagnostics: unknown parent construct (`KSEM118`), inheritance cycle (`KSEM119`).

A construct-backed declaration may also use a *prior declaration* as its parent, inheriting that
declaration's construct family and its implementations:

```kira
construct Drawable { requires { function draw() } }
Drawable Sprite { function draw() {} }   // first concrete child must implement draw()
Sprite Player {}                          // inherits draw()
Sprite AnimatedPlayer { function draw() {} }  // overrides draw()
```

## Properties

A construct declares a typed property schema. `required` properties must be provided by every
construct-backed declaration; others are optional. Properties are inherited through `extends`.

```kira
construct WebElement {
    properties {
        id: String
    }
}
construct Route extends WebElement {
    properties {
        required path: String
        title: String
    }
}
```

Declarations supply properties through a **section** (never a constructor-style call):

```kira
Route Home {
    properties {
        path: "/"
        id: "home"
    }
}
```

Diagnostics: missing required property (`KSEM123`), unknown property (`KSEM124`), property type
mismatch (`KSEM125`), duplicate property (`KSEM126`).

## Required functions

`requires { function ... }` lists functions a family's first concrete declaration must implement.
A required function returning `Result<Value, Failure>` is fallible (see `attempt`/`try`/`handle`).
`Self` refers to the implementing declaration's type.

```kira
construct WebElement {
    requires {
        function render() -> Result<DomNode, RenderFailure>
        function update(previous: Self)
        function unmount()
    }
}
```

An "interface-like" construct is simply a `requires`-only construct with no content.

Diagnostics: missing required function (`KSEM120`), required-function signature mismatch (`KSEM121`).

## Direct members: `@Required` and the `node` bridge

Alongside the section surface, a construct may declare **direct members** at its body's top level.
This is the SwiftUI-style UI surface: requirements are written as members, not as a separate
`requires { ... }` block.

- `@Required let name: T` — a field every concrete declaration must provide.
- `@Required function f(...) -> T` — a required behavior (a bodyless signature). Reuses the same
  satisfaction checking as `requires` (`KSEM120`/`KSEM121`).
- `let name: T { ... }` — a **computed default member** (a block-bodied field). Concrete
  declarations may override it, and it is not stored state.

```kira
construct Node {
    @Required function measure(proposal: SizeProposal) -> Size
    @Required function place(bounds: Rect) -> PlacedNode
}

construct Widget {
    @Required let body: Widget
    let node: Node { body.node }   // default Widget->Node bridge; composites inherit it
}
```

A concrete declaration uses the family name directly (`Widget Text { ... }`). Component inputs are
ordinary `let` fields. The typed Widget→Node bridge is `let node: Node { ... }`.

```kira
// Composite widget: provides `body`, inherits the default `node = body.node`.
Widget Button {
    let title: String = ""
    let body: Widget {
        Text(text: title)
    }
}

// Primitive/terminal widget: provides `node` directly and omits `body`.
Widget Text {
    let text: String = ""
    let node: Node {
        TextNode(text: text)
    }
}
```

**Terminal-`node` rule.** A required field need not be provided when the declaration overrides
every default member that consumes it. Because the default `node` reads `body`, an explicit
`node` discharges the required `body`, and node resolution stops at the explicit `node` rather
than recursing through `body`. A declaration that provides neither `body` nor `node` is rejected.

Diagnostics: missing required member (`KSEM140`); a declaration whose `body` expands to itself
without a terminal `node`, which would resolve forever (`KSEM141`).

## Caller-provided content: `@Content`

A concrete declaration marks caller-provided children as `@Content` fields. **Field names are the
channel names** — there are no string-labeled channels. A trailing `{ ... }` block on a
construction fills them, resolved by inspecting the callee declaration:

- One `@Content let x: Widget` → a bare block holds **exactly one** child.
- One `@Content let xs: [Widget]` → a bare block holds an **ordered list** of children.
- Several `@Content` fields → a bare block is ambiguous; children are supplied as **named fills**
  using the field names. Declaration order is preserved.

```kira
Widget VStack {
    @Content let children: [Widget]
    let node: Node { VStackNode() }
}

Widget Padding {
    @Content let child: Widget
    let node: Node { PaddingNode() }
}

Widget Dialog {
    @Content let header: Widget
    @Content let content: Widget
    let node: Node { DialogNode() }
}

Widget Screen {
    let body: Widget {
        VStack {
            Text(text: "title")
            HStack {
                Text(text: "left")
                Text(text: "right")
            }
            Dialog {
                header { Text(text: "Title") }
                content { Text(text: "Message") }
            }
        }
    }
}
```

Diagnostics: a trailing block on a declaration with no `@Content` field (`KSEM142`); wrong child
count for a single-`Widget` field (`KSEM143`); a bare block where named fills are required for
multiple `@Content` fields (`KSEM144`); a non-widget content child (`KSEM145`). String-labeled
channels such as `@Content("header")` are not part of the language (rejected as an annotation that
takes no parameters).

## Fluent modifiers: `extend`

`extend C { ... }` adds fluent modifier functions to a construct family — the external, chainable
surface (`.padding(...)`), distinct from the core Widget→Node bridge. Modifiers return the family
type and wrap the receiver via `self`.

```kira
extend Widget {
    function padding(amount: Float) -> Widget {
        Padding(amount: amount) {
            self
        }
    }
}

Widget Button {
    let title: String = ""
    let body: Widget {
        Text(text: title).padding(amount: 8)
    }
}
```

Diagnostics: extending an unknown construct (`KSEM146`).

> These checks run before any backend (constructions and modifier bodies are validated, not yet
> lowered to a runtime value). Executing the `node` bridge to produce real `Node` values — which
> requires heterogeneous `[Widget]` polymorphism — lands in a later stage.

## Content channels

A construct's `content { ... }` block declares named content channels. `accepts` constrains the
element type; `count` (a `min..max` or `min..` range) constrains how many elements a declaration
may place. Channels are inherited through `extends`.

```kira
construct WebElement {
    content {
        head {
            accepts Title
            count 0..1
        }
        body {
            accepts Title
            count 1..
        }
    }
}
```

A declaration fills channels with named sections:

```kira
Document Page {
    head { Title("Home") }
    body {
        Title("Welcome")
        Title("More")
    }
}
```

Diagnostics: unknown channel (`KSEM127`), count violation (`KSEM129`), accepts mismatch
(`KSEM130`), duplicate channel (`KSEM138`).

## Content composition

A construct may compose inherited content:

- `content sealed` — closes the content surface; descendants may not add/refine/project content.
- `content refine { channel { … } }` — narrows an inherited channel (count must stay within the
  inherited range; `accepts` may not change to a different element type).
- `content passthrough` — forwards the parent's channels; the construct owns none of its own.
- `content project { local as Parent.channel }` — maps a local declaration section name onto an
  ancestor construct's channel.

```kira
construct Route extends WebElement {
    content project {
        view as WebElement.content
        header as WebElement.head
    }
}
```

Diagnostics: content into a sealed construct (`KSEM128`), unknown projection target (`KSEM131`),
invalid composition — widening refine / meaningless passthrough (`KSEM132`).

## Linear error handling: `attempt` / `try` / `handle`

`attempt`/`try`/`handle` unwraps `Result<Value, Failure>`. Inside an `attempt` block, `try expr`
yields the `Ok` value and continues; on `Error` control transfers to the matching `handle` case,
selected by the failure's variant. `try` is valid only inside `attempt` (`handle` is a contextual
keyword). This construct **executes** (vm and hybrid backends).

```kira
attempt {
    let node = try element.render()
    append(target, node)
} handle {
    MissingNode {
        Log.error("missing node")
    }
    InvalidState {
        Log.error("invalid state")
    }
}
```

A `handle` case may bind and read the failure's payload (`MissingNode(reason)` then use `reason`);
the value is carried through correctly on every backend.

Rules and diagnostics: `try` outside `attempt` or in an unsupported position (`KSEM133`); `try` on
a non-`Result` value (`KSEM134`); a missing handle case for a reachable failure variant
(`KSEM135`); a handle case that is not a variant of the failure enum (`KSEM136`); incompatible
failure enums across the `try`s of one `attempt` (`KSEM137`). All `try`s in one `attempt` must
share a single failure enum.

`attempt`/`try`/`handle` and the nested-`match` it lowers to execute on **vm, llvm, and hybrid**
(`tests/pass/run/attempt_try_handle`, `tests/pass/run/nested_enum_payload_parity`).

## Notes

- Content-requiredness is expressed through content channels: a channel with `count 1..`
  requires at least one element, reported as a count violation (`KSEM129`) when a declaration
  leaves it empty. The older `requires { content; }` marker has been removed — `requires`
  now declares required *functions* only.
