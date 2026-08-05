---
name: verifying-work
description: "Read before claiming completion or committing. Run the full workspace validation gate, then add any required lint, backend-parity, or native-runtime proof."
---

# Verification

Do not accept launches, smoke output, placeholders, hardcoded success, or host-rendered substitutes as proof. Verify the changed Kira-owned path.

Run:

`kira_dev_validate` `{scope: "workspace", full: true, detail: "failures"}`

Narrow validation scopes are for iteration, not completion.

Run `kira_dev_validate` `{suite: "backend_parity"}` after lowering or semantics changes. Require matching stdout and exit status on VM and native.

Verify Kira lint changes with:

`KIRA_FOUNDATION_HOME=$PWD/foundation kira lint ../ui-foundation`

Run `kira_dev_build` `{scope: "workspace"}` after changes affecting native runtime linkage or `kira_rt_*` symbols so validation does not use a stale static archive.