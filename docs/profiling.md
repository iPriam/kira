# Profiling

`kira profile` records where a program spends its time and reads the recording
back. It is modelled on `perf`: the verbs, the flags, and the report columns are
the same wherever `perf` has an answer.

```sh
kira profile record                        # the package you are standing in
kira profile record app.kira --backend vm  # one backend
kira profile report                        # where the time went
kira profile report --machine              # what the machine was doing
kira profile annotate Grid.step            # inside one function
kira profile stat                          # the one-screen summary
```

## Verbs

| Verb | What it does |
| --- | --- |
| `record` | runs the program and writes a recording |
| `report` | time per function, flat or as a call graph |
| `annotate` | time per instruction inside one function |
| `script` | every sample and its stack |
| `stat` | the summary of a run |
| `diff` | what changed between two recordings |

A recording is one file. `record` writes `kira.profile` in the working
directory; every other verb reads it. `-o` and `-i` name another.

## Views

A recording holds up to two views of the same run.

| View | Frames | Flag |
| --- | --- | --- |
| Kira | the functions the program's author wrote | `--kira` (default) |
| machine | the interpreter, the runtime, the system, the program's machine code | `--machine` |

Where each view comes from is the only thing that differs between backends.

| Backend | Kira view | Machine view |
| --- | --- | --- |
| `vm` | the call stack the interpreter publishes | the interpreter running it |
| `hybrid` | the same, for the `@Runtime` half | both halves |
| `llvm` | the machine frames, recovered from the symbols the backend emitted | the same frames |

A native run has one view because its Kira frames *are* machine frames. Asking
for the Kira view of a recording that has none prints the machine view rather
than nothing.

## Events

| Event | Meaning |
| --- | --- |
| `cpu-clock` (default) | sampled by the platform's profiler |
| `instructions` | every interpreted instruction, counted exactly |

`-e instructions` runs the program with an observer on every instruction. It is
exact and slow, it answers "how many times did this run" rather than "how long
did this take", and it needs an interpreter — a native build has no interpreted
instructions to count.

## Collectors

Machine samples come from the platform's own profiler.

| Platform | Tool | Event recorded |
| --- | --- | --- |
| Linux | `perf record` / `perf script` | `cpu-clock` |
| macOS | `/usr/bin/sample` | `wall-clock` |
| Windows | DbgHelp stack walking | `cycles` |

A recording always launches the program it profiles rather than attaching to one
already running. Profiling your own child needs no elevated session on any of
the three.

A machine with none of these still records the Kira view, and says why the
machine view is missing.

## Reading a report

```text
# Recording: grid (backend vm, wall 4.85s, exit 0)
# View: kira   Event: kira-wall   Collector: kira-runtime   Frequency: 997 Hz
# Samples: 4082   Event count (approx.): 4.58s   Lost: 7
#
# Children      Self  Samples  Command   Shared Object    Symbol
# ........  ........  .......  ........  ...............  ......
#
    50.19%    50.19%     2089  grid      [vm]             [K] Grid.step
    49.81%    30.06%      882  grid      [vm]             [K] Grid.draw
```

`Children` is the share of samples the function appeared anywhere in; `Self` the
share it was the innermost frame of. The marker before each symbol says what
kind of code it is.

| Marker | Kind |
| --- | --- |
| `[K]` | a function written in Kira |
| `[R]` | Kira's runtime |
| `[.]` | other machine code in the program |
| `[C]` | a C library the program imported |
| `[k]` | the operating system |
| `[?]` | an address no symbol covered |

`--folded` writes collapsed stacks instead of a table, which every flame-graph
renderer reads.

## Cost

Publishing the Kira call stack costs two atomic stores per interpreted
instruction, and only in a run being profiled: the interpreter selects a
dispatch loop without them otherwise.
