#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdarg.h>
#include <string.h>

#if defined(__APPLE__)
#include <TargetConditionals.h>
#if TARGET_OS_IPHONE
#include <mach-o/loader.h>
const struct mach_header *_dyld_get_image_header_containing_address(const void *address) {
    (void)address;
    return NULL;
}
#endif
#endif

#if defined(_WIN32)
#define KIRA_BRIDGE_EXPORT __declspec(dllexport)
#include <fcntl.h>
#include <io.h>
#else
#define KIRA_BRIDGE_EXPORT
#endif

static void (*kira_runtime_invoker)(uint32_t) = NULL;
typedef struct {
    const unsigned char *ptr;
    size_t len;
} KiraBridgeString;

typedef enum {
    KIRA_BRIDGE_VALUE_VOID = 0,
    KIRA_BRIDGE_VALUE_INTEGER = 1,
    KIRA_BRIDGE_VALUE_FLOAT = 2,
    KIRA_BRIDGE_VALUE_STRING = 3,
    KIRA_BRIDGE_VALUE_BOOLEAN = 4,
    KIRA_BRIDGE_VALUE_RAW_PTR = 5
} KiraBridgeValueTag;

typedef union {
    int64_t integer;
    double float64;
    KiraBridgeString string;
    uint8_t boolean;
    uintptr_t raw_ptr;
} KiraBridgePayload;

typedef struct {
    uint8_t tag;
    uint8_t reserved[7];
    KiraBridgePayload payload;
} KiraBridgeValue;

typedef struct {
    size_t len;
    KiraBridgeValue *items;
} KiraArray;

/* Native-backend native-state token. These three fields are the C-ABI prefix shared with
 * the VM's `NativeStateBox` (packages/kira_vm_runtime/src/vm.zig). The VM appends VM-internal
 * metadata fields after this prefix, but tokens are never cast across backends: the native
 * path allocates/reads only this 3-field struct, and its `payload` is a raw byte buffer, while
 * the VM's payload holds Zig BridgeValue/Value arrays. A comptime assertion on the Zig side
 * enforces that this prefix layout stays in sync. */
typedef struct {
    uint64_t type_id;
    void *payload;
    void *runtime_payload;
} KiraNativeState;

static void (*kira_runtime_invoker_ex)(uint32_t, const KiraBridgeValue *, uint32_t, KiraBridgeValue *) = NULL;
static void *(*kira_array_alloc_fn)(size_t) = NULL;
static void (*kira_array_free_fn)(void *, size_t) = NULL;
static void (*kira_live_first_frame_hook)(void) = NULL;
static void (*kira_live_log_hook)(const char*) = NULL;
static int kira_trace_execution_enabled = -1;
#if defined(_WIN32)
static int kira_stdout_binary_configured = 0;
#endif

static void kira_prepare_stdout(void) {
#if defined(_WIN32)
    if (!kira_stdout_binary_configured) {
        _setmode(_fileno(stdout), _O_BINARY);
        kira_stdout_binary_configured = 1;
    }
#endif
}

static int kira_trace_enabled(void) {
    /*
     * Memoize the environment lookup. This is called from kira_trace_log on every
     * array release, print, and bridge op, so a fresh getenv() here (a locked,
     * linear scan of the process environment) on each call dominated the runtime of
     * allocation-heavy native programs — the per-operation trace check was the bulk
     * of `kira_array_release`'s self time under profiling. Resolve the env var once
     * and cache it; kira_set_execution_trace_enabled still overrides explicitly.
     */
    if (kira_trace_execution_enabled < 0) {
        const char *value = getenv("KIRA_TRACE_EXECUTION");
        kira_trace_execution_enabled =
            (value != NULL && value[0] != '\0' && value[0] != '0') ? 1 : 0;
    }
    return kira_trace_execution_enabled;
}

static void kira_trace_log(const char *domain, const char *event, const char *fmt, ...) {
    if (!kira_trace_enabled()) return;

    fprintf(stderr, "[trace][%s][%s] ", domain, event);
    va_list args;
    va_start(args, fmt);
    vfprintf(stderr, fmt, args);
    va_end(args);
    fputc('\n', stderr);
    fflush(stderr);
}

static int kira_bridge_probably_invalid_pointer(const void *ptr) {
    uintptr_t value = (uintptr_t)ptr;
    return value != 0 && value < 4096;
}

static void kira_array_repair_invalid_storage(KiraArray *array) {
    if (array == NULL) return;
    if (kira_bridge_probably_invalid_pointer(array->items)) {
        kira_trace_log("NATIVE", "ARRAY_REPAIR", "items=%p len=%llu", (void *)array->items, (unsigned long long)array->len);
        array->items = NULL;
        array->len = 0;
    }
}

static int kira_array_is_active(const KiraArray *array) {
    /*
     * Validity contract: reject only null and sentinel-small pointers. There is
     * deliberately no live-array registry to consult.
     *
     * A global linked-list registry (kira_active_arrays) used to exist here, but
     * once profiling removed the registry scan from this function the registry
     * became write-only dead weight: kira_array_register malloc'd a node on every
     * kira_array_alloc and kira_array_unregister was never called, so the list
     * grew unbounded — a genuine native memory leak under UI workloads. The whole
     * registry has been removed. Membership could never be the validity check
     * anyway: hybrid runtime calls pass arrays whose native layout was allocated
     * by the Zig VM bridge rather than by kira_array_alloc in this C helper, and
     * those borrowed arrays are valid for native reads, writes, and appends for
     * the duration of the call. The VM owns final destruction after it syncs the
     * borrowed layout back.
     */
    if (array == NULL || kira_bridge_probably_invalid_pointer(array)) return 0;
    return 1;
}

KIRA_BRIDGE_EXPORT void kira_set_execution_trace_enabled(uint8_t enabled) {
    kira_trace_execution_enabled = enabled != 0 ? 1 : 0;
}

KIRA_BRIDGE_EXPORT void kira_hybrid_install_array_allocator(void *(*alloc_fn)(size_t), void (*free_fn)(void *, size_t)) {
    kira_array_alloc_fn = alloc_fn;
    kira_array_free_fn = free_fn;
}

KIRA_BRIDGE_EXPORT void kira_live_install_first_frame_hook(void (*hook)(void)) {
    kira_live_first_frame_hook = hook;
}

KIRA_BRIDGE_EXPORT void kira_live_install_log_hook(void (*hook)(const char*)) {
    kira_live_log_hook = hook;
}

KIRA_BRIDGE_EXPORT void kira_live_emit_log_line(const char* line) {
    if (line == NULL) {
        return;
    }
    if (kira_live_log_hook != NULL) {
        kira_live_log_hook(line);
    }
    fprintf(stderr, "%s\n", line);
    fflush(stderr);
}

KIRA_BRIDGE_EXPORT void kira_live_emit_first_frame(void) {
    if (kira_live_first_frame_hook != NULL) {
        kira_live_first_frame_hook();
    }
}

static void *kira_bridge_alloc(size_t size) {
    if (size == 0) {
        size = 1;
    }
    if (kira_array_alloc_fn != NULL) {
        return kira_array_alloc_fn(size);
    }
    return malloc(size);
}

static void *kira_bridge_calloc(size_t count, size_t size) {
    const size_t total = count * size;
    void *ptr = kira_bridge_alloc(total);
    if (ptr != NULL) {
        memset(ptr, 0, total == 0 ? 1 : total);
    }
    return ptr;
}

static void kira_bridge_free(void *ptr, size_t size) {
    if (ptr == NULL) {
        return;
    }
    if (kira_array_free_fn != NULL) {
        kira_array_free_fn(ptr, size == 0 ? 1 : size);
        return;
    }
    free(ptr);
}

KIRA_BRIDGE_EXPORT void kira_native_write_i64(int64_t value) {
    kira_prepare_stdout();
    kira_trace_log("NATIVE", "PRINT", "i64");
    printf("%lld", (long long)value);
    fflush(stdout);
}

KIRA_BRIDGE_EXPORT void kira_native_write_f64(double value) {
    kira_prepare_stdout();
    kira_trace_log("NATIVE", "PRINT", "f64");
    printf("%g", value);
    fflush(stdout);
}

KIRA_BRIDGE_EXPORT void kira_native_write_string(const unsigned char *ptr, uint64_t len) {
    kira_prepare_stdout();
    kira_trace_log("NATIVE", "PRINT", "string len=%llu", (unsigned long long)len);
    fwrite(ptr, 1, (size_t)len, stdout);
    fflush(stdout);
}

KIRA_BRIDGE_EXPORT void kira_native_write_ptr(uintptr_t value) {
    kira_prepare_stdout();
    kira_trace_log("NATIVE", "PRINT", "ptr");
    printf("0x%llx", (unsigned long long)value);
    fflush(stdout);
}

KIRA_BRIDGE_EXPORT void kira_native_write_newline(void) {
    kira_prepare_stdout();
    fputc('\n', stdout);
    fflush(stdout);
}

KIRA_BRIDGE_EXPORT void kira_native_print_i64(int64_t value) {
    kira_native_write_i64(value);
    kira_native_write_newline();
}

KIRA_BRIDGE_EXPORT void kira_native_print_f64(double value) {
    kira_native_write_f64(value);
    kira_native_write_newline();
}

KIRA_BRIDGE_EXPORT void kira_native_print_string(const unsigned char *ptr, uint64_t len) {
    kira_native_write_string(ptr, len);
    kira_native_write_newline();
}

KIRA_BRIDGE_EXPORT KiraArray *kira_array_alloc(int64_t len) {
    if (len < 0) return NULL;
    KiraArray *array = (KiraArray *)kira_bridge_alloc(sizeof(KiraArray));
    if (array == NULL) return NULL;
    array->len = (size_t)len;
    array->items = array->len == 0 ? NULL : (KiraBridgeValue *)kira_bridge_calloc(array->len, sizeof(KiraBridgeValue));
    return array;
}

/*
 * Deep clone for the borrow->owned boundary (pure-native path). The LLVM backend
 * emits this when an owned value is produced from borrowed data (e.g. returning a
 * struct element read out of a borrowed array), so the new owner gets independent
 * array storage instead of aliasing the borrowed array. `clone_elem`, when
 * provided, deep-clones each RAW_PTR element (an array of heap structs); a null
 * `clone_elem` copies elements byte-for-byte (primitive/leaf element types).
 * Ownership model only — no reference counts.
 */
KIRA_BRIDGE_EXPORT KiraArray *kira_array_clone(const KiraArray *array, void *(*clone_elem)(void *)) {
    if (kira_array_alloc_fn != NULL) return (KiraArray *)array; /* hybrid: VM owns; no native clone */
    if (array == NULL || kira_bridge_probably_invalid_pointer(array)) return NULL;
    KiraArray *copy = (KiraArray *)kira_bridge_alloc(sizeof(KiraArray));
    if (copy == NULL) return NULL;
    copy->len = array->len;
    if (array->len == 0 || array->items == NULL || kira_bridge_probably_invalid_pointer(array->items)) {
        copy->len = array->len;
        copy->items = NULL;
        return copy;
    }
    copy->items = (KiraBridgeValue *)kira_bridge_calloc(array->len, sizeof(KiraBridgeValue));
    if (copy->items == NULL) { copy->len = 0; return copy; }
    for (size_t i = 0; i < array->len; i++) {
        copy->items[i] = array->items[i];
        if (clone_elem != NULL && array->items[i].tag == KIRA_BRIDGE_VALUE_RAW_PTR) {
            void *element = (void *)array->items[i].payload.raw_ptr;
            if (element != NULL) copy->items[i].payload.raw_ptr = (uintptr_t)clone_elem(element);
        }
    }
    return copy;
}

KIRA_BRIDGE_EXPORT int64_t kira_array_len(const KiraArray *array) {
    if (!kira_array_is_active(array)) return 0;
    if (kira_bridge_probably_invalid_pointer(array->items)) return 0;
    return (int64_t)array->len;
}

KIRA_BRIDGE_EXPORT void kira_array_store(KiraArray *array, int64_t index, const KiraBridgeValue *value) {
    if (!kira_array_is_active(array)) return;
    kira_array_repair_invalid_storage(array);
    if (array == NULL || index < 0 || (size_t)index >= array->len) return;
    if (value == NULL) return;
    array->items[index] = *value;
}

/*
 * Drop-before-overwrite store. Overwriting an element whose slot owns heap
 * contents (a struct element with its own array/struct fields) must destroy the
 * prior occupant, or it orphans every overwrite (the P2 element-overwrite leak).
 * `release_raw_ptr` is the element destructor (e.g. kira_destroy_Node); a null fn
 * means primitive elements with nothing to drop, degrading to a plain store. The
 * old-vs-new pointer guard makes storing the same element back a no-op rather than
 * a use-after-free. Mirrors the per-element destroy loop in kira_array_release so
 * an element is reclaimed exactly once whether the array is released wholesale or
 * a slot is overwritten. Ownership model, no refcounts — see kira_array_release.
 */
KIRA_BRIDGE_EXPORT void kira_array_store_release(KiraArray *array, int64_t index, const KiraBridgeValue *value, void (*release_raw_ptr)(void *)) {
    if (!kira_array_is_active(array)) return;
    kira_array_repair_invalid_storage(array);
    if (array == NULL || index < 0 || (size_t)index >= array->len) return;
    if (value == NULL) return;
#if defined(KIRA_ARRAY_OWNERSHIP_FREE)
    /*
     * Defer on the hybrid/VM path exactly as kira_array_release does: the VM owns
     * and reclaims array memory through its own native-layout destructors.
     */
    if (kira_array_alloc_fn == NULL && release_raw_ptr != NULL &&
        array->items[index].tag == KIRA_BRIDGE_VALUE_RAW_PTR) {
        void *old = (void *)array->items[index].payload.raw_ptr;
        void *incoming = value->tag == KIRA_BRIDGE_VALUE_RAW_PTR ? (void *)value->payload.raw_ptr : NULL;
        if (old != NULL && old != incoming) release_raw_ptr(old);
    }
#else
    (void)release_raw_ptr;
#endif
    array->items[index] = *value;
}

/*
 * Drop-before-overwrite for an owned ARRAY FIELD. Reassigning `obj.arr = newArr`
 * orphans the old array (the P2 field-overwrite leak: 16-byte KiraArray headers).
 * The backend emits this with the old and incoming array pointers and the element
 * destructor; it releases the old array unless it is null (moved-out/uninitialised
 * field) or the same pointer being stored back. Delegates to kira_array_release, so
 * it inherits the KIRA_ARRAY_OWNERSHIP_FREE gate and the hybrid/VM deferral. Only
 * sound because aggregate reads now deep-clone (value semantics) — the old field
 * value is independently owned and not aliased by the incoming value.
 */
KIRA_BRIDGE_EXPORT void kira_array_release(KiraArray *array, void (*release_raw_ptr)(void *));

KIRA_BRIDGE_EXPORT void kira_array_release_replaced(KiraArray *old_array, KiraArray *incoming, void (*release_raw_ptr)(void *)) {
    if (old_array == NULL || old_array == incoming) return;
    /*
     * Defense-in-depth. A struct array field is an untagged raw KiraArray* (unlike a
     * tagged bridge-value element), so a non-heap value in the field would be freed
     * blindly — this aborted on device (0x4628d3 in a foundation FFI struct). The
     * backend already restricts this call to non-FFI struct types whose array fields
     * are always kira_array_alloc'd, but guard anyway: kira_bridge_alloc returns at
     * least 16-byte-aligned pointers, so reject anything unaligned or in the low
     * sentinel range rather than free a value that was never allocated.
     */
    uintptr_t bits = (uintptr_t)old_array;
    if (bits < 0x1000 || (bits & 0xF) != 0) return;
    kira_array_release(old_array, release_raw_ptr);
}

/*
 * Free an owned closure value. A closure i64 is either a callable-value (a bare
 * function id, high bit clear, within u32 — no heap) or a tagged heap closure block
 * { i64 fn_id; i64 count; KiraBridgeValue[] } with the high bit set. Used to drop an
 * owned closure parameter at the callee's scope exit. Tag-safe and null/sentinel-safe
 * so it also accepts plain heap raw pointers (high bit already clear). Captured heap
 * values are left untouched (ambiguous without per-capture type info — conservative:
 * leak rather than risk freeing a shared/static capture or a double free).
 */
KIRA_BRIDGE_EXPORT void kira_destroy_closure(uintptr_t value) {
    if (value == 0) return;
    if (value <= 0xFFFFFFFFULL) return; /* callable-value function id: nothing to free */
    /*
     * Only an actual closure block carries the high tag bit (set in lowerConstClosure).
     * An owned raw_ptr parameter that is NOT a closure — e.g. an FFI/native-state userdata
     * pointer passed as `RawPtr` — has the high bit clear and must NOT be freed: it is
     * owned by the caller (the native-state box), and freeing it corrupts that box (seen as
     * "userdata type mismatch" on a later nativeRecover). The high bit cleanly separates a
     * real closure value from a borrowed raw pointer, so this is safe for both.
     */
    if ((value & 0x8000000000000000ULL) == 0) return;
    void *ptr = (void *)(value & 0x7FFFFFFFFFFFFFFFULL); /* clear the closure tag bit */
    uintptr_t bits = (uintptr_t)ptr;
    if (bits < 0x1000 || (bits & 0x7) != 0) return; /* not a heap-allocated block */
    free(ptr);
}

KIRA_BRIDGE_EXPORT void kira_array_append(KiraArray *array, const KiraBridgeValue *value) {
    if (!kira_array_is_active(array)) return;
    kira_array_repair_invalid_storage(array);
    if (array == NULL || value == NULL) return;
    size_t next_len = array->len + 1;
    KiraBridgeValue *next_items = (KiraBridgeValue *)kira_bridge_alloc(next_len * sizeof(KiraBridgeValue));
    if (next_items == NULL) return;
    if (array->items != NULL && array->len != 0) {
        memcpy(next_items, array->items, array->len * sizeof(KiraBridgeValue));
        kira_bridge_free(array->items, array->len * sizeof(KiraBridgeValue));
    }
    array->items = next_items;
    array->items[array->len] = *value;
    array->len = next_len;
}

KIRA_BRIDGE_EXPORT void kira_array_load(const KiraArray *array, int64_t index, KiraBridgeValue *out_value) {
    KiraBridgeValue zero = {0};
    if (out_value == NULL) return;
    if (!kira_array_is_active(array) || kira_bridge_probably_invalid_pointer(array->items) || index < 0 || (size_t)index >= array->len) {
        *out_value = zero;
        return;
    }
    *out_value = array->items[index];
}

KIRA_BRIDGE_EXPORT void kira_array_release(KiraArray *array, void (*release_raw_ptr)(void *)) {
    if (!kira_array_is_active(array)) {
        kira_trace_log("NATIVE", "ARRAY_RELEASE_SKIP", "array=%p", (void *)array);
        return;
    }

    /*
     * Hybrid path: the VM owns and reclaims array memory via its own
     * native-layout destructors, and VM arrays may not carry a refcount field.
     * Defer, and never touch the refcount.
     */
    if (kira_array_alloc_fn != NULL) {
        kira_trace_log("NATIVE", "ARRAY_RELEASE_DEFERRED", "array=%p len=%llu", (void *)array, (unsigned long long)array->len);
        return;
    }

    kira_array_repair_invalid_storage(array);

    /*
     * Ownership model (no reference counts). The LLVM backend, driven by the borrow
     * checker, emits exactly one release at each owned array's drop point; moves
     * transfer ownership (the source is not dropped) and borrows are never dropped,
     * while owned values produced from borrowed data are deep-cloned (kira_array_clone)
     * so they own independent storage. A release here is then the sole owner going
     * away: run the element destructor on RAW_PTR elements, then free the items
     * buffer and the struct.
     *
     * Gated behind KIRA_ARRAY_OWNERSHIP_FREE (default OFF = defer = stable, the
     * committed no-crash behavior). Turning it on requires the backend to (a) deep
     * clone at EVERY borrow->owned boundary (currently only return-struct copies) and
     * (b) emit drops for orphaned owned arrays (field move-outs). Until both are
     * complete and DEVICE-validated, freeing risks the use-after-free that crashed on
     * device. See .codex/work/reports/array-registry-leak-and-promotion.md §7f.
     */
#if defined(KIRA_ARRAY_OWNERSHIP_FREE)
    kira_trace_log("NATIVE", "ARRAY_RELEASE_FREE", "array=%p len=%llu", (void *)array, (unsigned long long)array->len);
    if (release_raw_ptr != NULL && array->items != NULL &&
        !kira_bridge_probably_invalid_pointer(array->items)) {
        for (size_t i = 0; i < array->len; i++) {
            if (array->items[i].tag == KIRA_BRIDGE_VALUE_RAW_PTR) {
                void *element = (void *)array->items[i].payload.raw_ptr;
                if (element != NULL) release_raw_ptr(element);
            }
        }
    }
    kira_bridge_free(array->items, array->len * sizeof(KiraBridgeValue));
    kira_bridge_free(array, sizeof(KiraArray));
    return;
#else
    (void)release_raw_ptr;
    kira_trace_log("NATIVE", "ARRAY_RELEASE_DEFERRED", "array=%p len=%llu", (void *)array, (unsigned long long)array->len);
#endif
}

KIRA_BRIDGE_EXPORT KiraNativeState *kira_native_state_alloc(uint64_t type_id, int64_t payload_size) {
    if (payload_size < 0) return NULL;
    KiraNativeState *state = (KiraNativeState *)calloc(1, sizeof(KiraNativeState));
    if (state == NULL) return NULL;
    state->type_id = type_id;
    state->payload = payload_size == 0 ? NULL : calloc(1, (size_t)payload_size);
    state->runtime_payload = NULL;
    if (payload_size != 0 && state->payload == NULL) {
        free(state);
        return NULL;
    }
    return state;
}

KIRA_BRIDGE_EXPORT void *kira_struct_alloc(uint64_t type_id, size_t size) {
    unsigned char *base = (unsigned char *)malloc(sizeof(uint64_t) + size);
    if (base == NULL) return NULL;
    *((uint64_t *)base) = type_id;
    void *payload = (void *)(base + sizeof(uint64_t));
    memset(payload, 0, size);
    return payload;
}

KIRA_BRIDGE_EXPORT uint64_t kira_struct_type_id(void *ptr) {
    if (ptr == NULL) return 0;
    return *(((uint64_t *)ptr) - 1);
}

KIRA_BRIDGE_EXPORT void kira_struct_free(void *ptr) {
    if (ptr == NULL) return;
    free(((unsigned char *)ptr) - sizeof(uint64_t));
}

KIRA_BRIDGE_EXPORT void *kira_native_state_payload(KiraNativeState *state) {
    if (state == NULL) return NULL;
    return state->payload;
}

KIRA_BRIDGE_EXPORT void *kira_native_state_recover(void *user_data, uint64_t expected_type_id) {
    KiraNativeState *state = (KiraNativeState *)user_data;
    if (state == NULL) {
        fprintf(stderr, "kira native state recovery failed: userdata was null\n");
        abort();
    }
    if (state->type_id != expected_type_id) {
        fprintf(stderr, "kira native state recovery failed: userdata type mismatch\n");
        abort();
    }
    return state->payload;
}

KIRA_BRIDGE_EXPORT void kira_hybrid_install_runtime_invoker(void (*invoker)(uint32_t, const KiraBridgeValue *, uint32_t, KiraBridgeValue *)) {
    kira_runtime_invoker_ex = invoker;
}

KIRA_BRIDGE_EXPORT void kira_hybrid_call_runtime(uint32_t function_id, const KiraBridgeValue *args, uint32_t arg_count, KiraBridgeValue *out_result) {
    kira_trace_log("TRAMPOLINE", "ENTER", "native->runtime fn=%u args=%u", function_id, arg_count);
    if (kira_runtime_invoker_ex != NULL) {
        kira_runtime_invoker_ex(function_id, args, arg_count, out_result);
        if (out_result != NULL) {
            kira_trace_log("TRAMPOLINE", "RETURN", "runtime->native fn=%u tag=%u", function_id, (unsigned)out_result->tag);
        } else {
            kira_trace_log("TRAMPOLINE", "RETURN", "runtime->native fn=%u", function_id);
        }
        return;
    }
    if (kira_runtime_invoker != NULL) {
        kira_runtime_invoker(function_id);
        kira_trace_log("TRAMPOLINE", "RETURN", "runtime->native fn=%u", function_id);
    }
}
