---
name: working-on-macos
description: "What is missing or different in a macOS shell here: no timeout/gtimeout, BSD flags not GNU, and how to bound a command that may hang. Read before writing any shell command, test harness, or CI step that runs on macOS."
---

# Working on macOS

## timeout

Neither `timeout` nor `gtimeout` exists. Both are GNU coreutils; macOS ships
BSD. Reaching for one costs a round trip and returns `command not found`.

Bound a command that may hang with perl:

```sh
perl -e 'alarm shift; exec @ARGV or die "exec: $!"' 60 <command>
```

The `or die` matters, or a missing binary exits 0.

Better: make the hang impossible. A test that spawns a process kills it on
drop, so it fails instead of hanging.

## BSD flags

Assume BSD, not GNU: `sed -i ''` takes an explicit empty suffix, and `sed -r`
does not exist (use `-E`).
