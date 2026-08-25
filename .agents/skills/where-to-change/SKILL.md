---
name: where-to-change
description: "Read when crate ownership is unclear or before adding a crate dependency. Place changes in the lowest owning layer, preserve dependency direction, and keep binaries as leaves."
---

# Crate ownership

Place a change in the lowest crate that owns its contract. Read candidate crates' layer markers in `lib.rs`.

Normal dependencies may point only downward. Put test-only upward references in `[dev-dependencies]`.

Put shared types in model or interface crates and behavior above them. When lower code needs behavior from a higher layer, define the trait at the lower boundary and implement it above.

Keep `kira-core`, `kira-source`, and model crates limited to shared contracts. Do not move changing implementation into them.

Keep binary crates thin. Move reusable logic into library crates below them.

Define each contract once in its owning crate. Re-export or alias it elsewhere.

## Boundaries

`kira-main` owns the embedding surface.

`kira-project::autobind` owns binding contents. `kira-build::autobind` owns when binding generation runs.

Runner crates consume `.klbundle` and `kira-live`; they must not depend on compiler IR, semantics, or backends.

Renderer backend work belongs in `kira-graphics`. This workspace owns shader code generation and its native bridge.