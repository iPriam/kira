# Survey: Any/erasure/runtime Type and async/tasks (2026-09-01)

Any: `assignable_to` exclusions (ty/mod.rs:247 Void, 251 Task/MainThreadTask, 256 Cell);
`erases_into_any` (427); `moves_on_bind(Any)=false` (455); `IntoAny` inserted coercion.rs:100-110;
`ErasedTypeId` family<<32|row (ty/erased.rs:66-108; None for Void/Distinct/CString/CBlock/Cell/
Task/NativeState/Any → backend internal errors compile/expression.rs:370, boxing.rs:76; RawPtr/
ForeignPtr collapse :92). VM `Erase` opcode 0x65, `Object::Erased{type_id,payload,shares}`
(value/mod.rs:236-257); native box = enum box (codegen/boxing.rs:47-84), `kira_rt_any_eq` →
`boxes_equal` (native-bridge enums/mod.rs:378-425). Hybrid `BridgeData::Any` (runtime-abi
bridge.rs:129-132), `NativeStateValue::Any` (native_state.rs:86,262-266). No is/as/.type/
TypeCastError; `attempt` catches `try` results only (stmt/attempts.rs). Tests pinning old
behavior: tests/any.rs:77 (KSEM020 narrowing), tests/tasks.rs:58, backend_parity/any.rs.

Tasks: `async` parsed (kira-parser item.rs:79-84), `HirFunction.is_async` never read; direct
calls allowed and pinned (backend_parity/tasks.rs:39, tests/tasks.rs:15, AsyncSpine.kira:34,
AsyncSpineTests.kira:9-27). `analyze_task_spawn`/`analyze_task_call` (kira-semantics tasks.rs:44-197)
use `lookup_function` first-by-name, Int/Float params only (`task_scalar` 35-41), TASK_SLOTS=8,
Send gate 202-234; handle surface 252-311; scheduler is generated IR (kira-ir tasks.rs; yield
nests drive on stack :12-20). `TaskExecutor` (runtime-abi tasks.rs:190-272): 1-based indices, no
generations/reclamation, `pick_ready` oldest-first (304-313). VM executor per Interp
(interp.rs:118-124); native thread-local `kira_rt_task_op` exits process (native-bridge tasks.rs:
53,59); hybrid resets per run (hybrid-runtime library.rs:501-505). Slicing only for lifecycle
fibers (fiber.rs). Main thread ops (runtime-abi main_thread.rs:19-70), KSEM331-336
(typeck/calls/main_thread.rs), loops vm main_thread.rs:240-330 / native main_thread.rs.
`process::exit` sites: native-bridge tasks.rs:53,59; traps.rs; runtime.rs:452,464,506,523,555,630;
array.rs:492,504; accounting.rs:189,196; string_ops.rs:179; native_state.rs:653; main_thread.rs:589;
kira-libffi native.rs:196.
