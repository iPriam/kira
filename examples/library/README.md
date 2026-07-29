# A Kira library called from Rust

`greetings.kira` is authored in Kira and consumed from Rust. Build it, depend on
the crate it generates, call it.

```sh
kira build greetings.kira                  # --backend vm is the default
```

```text
.kira-build/lib/greetings.kbc               the artifact
.kira-build/rust/greetings/                 the crate a Rust program depends on
```

```toml
[dependencies]
greetings = { path = "../greetings/.kira-build/rust/greetings" }
```

```rust
use greetings::{Greeter, Greetings};

fn main() -> Result<(), greetings::Error> {
    let lib = Greetings::load()?;
    let greeter: Greeter = lib.make_greeter("kira")?;   // a handle out
    println!("{}", lib.greeting(&greeter)?);            // an owned String out
    assert_eq!(lib.greeter_width(&greeter)?, 40);
    let wider = lib.widen(&greeter, 20)?;               // a handle in and out
    assert_eq!(lib.greeter_width(&wider)?, 60);
    drop(greeter);                                      // releases the object
    Ok(())
}
```

Consumer names are derived by snake-casing, so `makeGreeter` is reached as
`make_greeter`. Each exported class becomes a Rust newtype over a handle, and
dropping one releases the Kira object it names — nothing else does.

## The same five lines against a different engine

```sh
kira build --backend llvm greetings.kira     # a static archive the crate links
kira build --backend hybrid greetings.kira   # a bytecode half plus a native one
```

The consumer's code does not change. All three engines write the same
`.kira-build/rust/greetings/` and generate the same public API; what differs is
entirely underneath — embedded bytecode, `kira_lib_*` trampolines in an archive,
or bytecode plus a shared library opened at load.

## Capturing the library's output

`announce` calls `print`, and the VM hands each finished line to the host the
embedder supplied. `Greetings::load()` uses a host that writes to stdout;
`load_with` takes your own, which is how a consumer keeps library output out of
its own stdout.

## What this example does not show

Arrays, structs and enums by value, and function values do not cross the export
boundary — each is refused in analysis by name, with its reason. `kira run` on
this package is refused too: a library has no `@Main`, because it is entered by
whatever consumes it.

The generated crate is a build artifact. It is regenerated on every build, never
committed, and its dependency paths are true only of the checkout that produced
it.

The repo README's "Library packages" section is the reference for all of this;
`crates/kira-export-consumer` is the same shape wired up as a test that runs
against all three engines.
