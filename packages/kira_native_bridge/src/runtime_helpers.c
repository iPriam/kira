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

typedef struct KiraArrayRegistryNode {
    KiraArray *array;
    struct KiraArrayRegistryNode *next;
} KiraArrayRegistryNode;

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
static KiraArrayRegistryNode *kira_active_arrays = NULL;
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
    if (kira_trace_execution_enabled >= 0) return kira_trace_execution_enabled;
    const char *value = getenv("KIRA_TRACE_EXECUTION");
    return value != NULL && value[0] != '\0' && value[0] != '0';
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

static void kira_array_register(KiraArray *array) {
    if (array == NULL) return;
    KiraArrayRegistryNode *node = (KiraArrayRegistryNode *)malloc(sizeof(KiraArrayRegistryNode));
    if (node == NULL) return;
    node->array = array;
    node->next = kira_active_arrays;
    kira_active_arrays = node;
}

static int kira_array_is_active(const KiraArray *array) {
    if (array == NULL || kira_bridge_probably_invalid_pointer(array)) return 0;
    for (KiraArrayRegistryNode *node = kira_active_arrays; node != NULL; node = node->next) {
        if (node->array == array) return 1;
    }
    /*
     * Hybrid runtime calls can pass arrays whose native layout was allocated by
     * the Zig VM bridge rather than by kira_array_alloc in this C helper. Those
     * borrowed arrays are still valid for native reads, writes, and appends for
     * the duration of the call, so registry membership cannot be the validity
     * check. Keep rejecting null and sentinel-small pointers, and let the VM own
     * final destruction after it syncs the borrowed layout back.
     */
    return 1;
}

static int kira_array_unregister(KiraArray *array) {
    if (array == NULL) return 0;
    KiraArrayRegistryNode **cursor = &kira_active_arrays;
    while (*cursor != NULL) {
        if ((*cursor)->array == array) {
            KiraArrayRegistryNode *node = *cursor;
            *cursor = node->next;
            free(node);
            return 1;
        }
        cursor = &(*cursor)->next;
    }
    return 0;
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

KIRA_BRIDGE_EXPORT void kira_native_write_string(const unsigned char *ptr, size_t len) {
    kira_prepare_stdout();
    kira_trace_log("NATIVE", "PRINT", "string len=%llu", (unsigned long long)len);
    fwrite(ptr, 1, len, stdout);
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

KIRA_BRIDGE_EXPORT void kira_native_print_string(const unsigned char *ptr, size_t len) {
    kira_native_write_string(ptr, len);
    kira_native_write_newline();
}

KIRA_BRIDGE_EXPORT KiraArray *kira_array_alloc(int64_t len) {
    if (len < 0) return NULL;
    KiraArray *array = (KiraArray *)kira_bridge_alloc(sizeof(KiraArray));
    if (array == NULL) return NULL;
    array->len = (size_t)len;
    array->items = array->len == 0 ? NULL : (KiraBridgeValue *)kira_bridge_calloc(array->len, sizeof(KiraBridgeValue));
    kira_array_register(array);
    return array;
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
    (void)release_raw_ptr;
    if (!kira_array_is_active(array)) {
        kira_trace_log("NATIVE", "ARRAY_RELEASE_SKIP", "array=%p", (void *)array);
        return;
    }
    kira_array_repair_invalid_storage(array);
    kira_trace_log("NATIVE", "ARRAY_RELEASE_DEFERRED", "array=%p len=%llu", (void *)array, (unsigned long long)array->len);
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
