/*
 * Kira runtime dynamic-FFI surface.
 *
 * This header is the canonical declaration of the `kira_runtime` dynamic-FFI
 * API. `dynamic_ffi_helpers.c` includes it so the implementation cannot drift,
 * and the Kira AutoBinder generates the Foundation `bindings/dynamicffi`
 * module from it so no Kira source ever hand-declares these functions.
 */
#ifndef KIRA_DYNAMIC_FFI_H
#define KIRA_DYNAMIC_FFI_H

#include <stdint.h>

#if defined(_WIN32)
#define KIRA_BRIDGE_EXPORT __declspec(dllexport)
#else
#define KIRA_BRIDGE_EXPORT
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* Host platform code: 1 = Windows, 2 = Linux, 3 = macOS, 0 = unknown. */
KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_host_platform_code(void);

/* Dynamic library loading. */
KIRA_BRIDGE_EXPORT void *kira_dynamic_library_open(const char *name);
KIRA_BRIDGE_EXPORT void *kira_dynamic_library_symbol(void *library, const char *name);
KIRA_BRIDGE_EXPORT void kira_dynamic_library_close(void *library);

/* Pointer helpers. */
KIRA_BRIDGE_EXPORT void *kira_dynamic_null_ptr(void);
KIRA_BRIDGE_EXPORT uint8_t kira_dynamic_ptr_is_null(void *ptr);

/* Native memory management. */
KIRA_BRIDGE_EXPORT void *kira_dynamic_alloc(uint64_t size);
KIRA_BRIDGE_EXPORT void kira_dynamic_free(void *ptr);

/* Scalar reads. */
KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_read_u32(void *ptr);
KIRA_BRIDGE_EXPORT int32_t kira_dynamic_read_i32(void *ptr);
KIRA_BRIDGE_EXPORT void *kira_dynamic_read_ptr(void *ptr);

/* Scalar reads at byte offsets. */
KIRA_BRIDGE_EXPORT uint8_t kira_dynamic_read_u8_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT uint16_t kira_dynamic_read_u16_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_read_u32_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT int32_t kira_dynamic_read_i32_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT uint64_t kira_dynamic_read_u64_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT int64_t kira_dynamic_read_i64_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT void *kira_dynamic_read_ptr_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT float kira_dynamic_read_f32_at(void *ptr, uint64_t offset);
KIRA_BRIDGE_EXPORT double kira_dynamic_read_f64_at(void *ptr, uint64_t offset);

/* Scalar writes. */
KIRA_BRIDGE_EXPORT void kira_dynamic_write_u32(void *ptr, uint32_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_ptr(void *ptr, void *value);

/* Scalar writes at byte offsets. */
KIRA_BRIDGE_EXPORT void kira_dynamic_write_u8_at(void *ptr, uint64_t offset, uint8_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_u16_at(void *ptr, uint64_t offset, uint16_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_u32_at(void *ptr, uint64_t offset, uint32_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_u64_at(void *ptr, uint64_t offset, uint64_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_i64_at(void *ptr, uint64_t offset, int64_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_ptr_at(void *ptr, uint64_t offset, void *value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_f32_at(void *ptr, uint64_t offset, float value);
KIRA_BRIDGE_EXPORT void kira_dynamic_write_f64_at(void *ptr, uint64_t offset, double value);

/* C string helpers. */
KIRA_BRIDGE_EXPORT void *kira_dynamic_cstring_dup(const char *text);
KIRA_BRIDGE_EXPORT const char *kira_dynamic_cstring_at(void *ptr, uint64_t offset);

/*
 * Generic libffi-backed call.
 *
 * `arg_types` points at `arg_count` uint32_t type tags, `arg_values` points at
 * `arg_count` pointer-sized value slots, and `result_out` receives the raw
 * result. Type tags: 0 void, 1 i8, 2 u8, 3 i16, 4 u16, 5 i32, 6 u32, 7 i64,
 * 8 u64, 9 f32, 10 f64, 11 pointer. Returns 0 on success or a nonzero
 * dynamic-FFI status code.
 */
KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_call(
    void *function_ptr,
    uint32_t result_type,
    const uint32_t *arg_types,
    const void *arg_values,
    uint32_t arg_count,
    void *result_out
);

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_last_error_code(void);

/*
 * Dynamic call builder.
 *
 * Allocates a typed argument list, appends arguments one by one, and invokes a
 * native function pointer through libffi. `kira_dynamic_call_new` returns an
 * opaque call handle (NULL on failure). Invoke functions return the call result
 * coerced to the named type; check `kira_dynamic_ffi_last_error_code` after an
 * invoke to distinguish a real zero result from a failed call. The handle can
 * be reused after `kira_dynamic_call_reset` and must be released with
 * `kira_dynamic_call_free`.
 */
KIRA_BRIDGE_EXPORT void *kira_dynamic_call_new(uint32_t max_args);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_reset(void *call);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_free(void *call);

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_ptr(void *call, void *value);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_i32(void *call, int32_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_u32(void *call, uint32_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_i64(void *call, int64_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_u64(void *call, uint64_t value);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_f32(void *call, float value);
KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_f64(void *call, double value);

KIRA_BRIDGE_EXPORT void kira_dynamic_call_invoke_void(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT int32_t kira_dynamic_call_invoke_i32(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_call_invoke_u32(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT int64_t kira_dynamic_call_invoke_i64(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT uint64_t kira_dynamic_call_invoke_u64(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT void *kira_dynamic_call_invoke_ptr(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT float kira_dynamic_call_invoke_f32(void *call, void *function_ptr);
KIRA_BRIDGE_EXPORT double kira_dynamic_call_invoke_f64(void *call, void *function_ptr);

/* Fixed-shape compatibility wrappers retained for existing generated bindings. */
KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_call_i32_ptr(void *function_ptr, void *arg0);
KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_call_i32_ptr_u32_ptr_ptr(
    void *function_ptr,
    void *arg0,
    uint32_t arg1,
    void *arg2,
    void *arg3
);

#ifdef __cplusplus
}
#endif

#endif /* KIRA_DYNAMIC_FFI_H */
