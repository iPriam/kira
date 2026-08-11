# Debug lab

This is a deliberately non-trivial Kira program: a cart is assembled from an
array of structs, pricing is reduced through several functions, and tax,
shipping, and risk are scheduled as cooperative async tasks. The healthy
program is `main.kira`. `buggy.kira` is a recoverable debugging fixture with
three marked logic bugs.

Run the healthy application:

```text
kira run --backend vm examples/debug-lab/main.kira
```

It should end with `889` followed by `PASS`. The same `main.kira` is included
in the repository's VM/LLVM/hybrid example-parity sweep.

Run the broken fixture to see the component-level failure:

```text
kira run --backend vm examples/debug-lab/buggy.kira
```

The expected total is still `889`, but the broken pipeline reports `981` and
ends with `FAIL`. The useful VM debugger stops are the three stage functions:

```text
kira debug --backend vm --batch --break discountAmount examples/debug-lab/buggy.kira
kira debug --backend vm --batch --break calculateTax examples/debug-lab/buggy.kira
kira debug --backend vm --batch --break calculateShipping examples/debug-lab/buggy.kira
```

To put the VM entirely under real LLDB, use the same breakpoints with
`--lldb`. LLDB stops in `kira_vm_debug_probe`; `rcx`/`rdx` on Windows x86-64
carry the VM function id and bytecode PC, while `r8` carries the opcode:

```text
kira debug --backend vm --lldb --batch --no-disassemble --break discountAmount examples/debug-lab/buggy.kira
```

At every batch stop, LLDB reads the VM's exported text mirror, so the
transcript includes the actual Kira locals, operand stack, instruction bytes,
and VM backtrace. For the tax bug, the useful instruction stop is:

```text
kira debug --backend vm --lldb --batch --no-disassemble --break calculateTax:5 examples/debug-lab/buggy.kira
```

That stop shows locals `[0]=900`, `[1]=8` and operand-stack `[900, 8, 1]`
before `AddInt`: the bad `rate + 1` is visible in the VM state itself. The
corrected program stops at `calculateTax:4` with locals `[0]=810`, `[1]=8`
and stack `[810, 8]` before `MulInt`. After changing the three expressions to
match `main.kira`, run the fixed program and confirm the final `PASS`.

On Windows, the stable multi-stop frontend is LLDB DAP:

```text
kira debug --backend vm --lldb-dap --dap-continues 2 --no-disassemble --break calculateTax:5 examples/debug-lab/buggy.kira
```

It verifies the native probe breakpoint and reads the same state through LLDB's
`evaluate` and `readMemory` requests, so the values above are debugger memory,
not a Kira-side log.
