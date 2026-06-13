/* The canonical declaration of this API ships with Foundation so the AutoBinder
 * can generate the `bindings/dynamicffi` module from it at user sites. Including
 * it here keeps the implementation and the generated bindings in lockstep. */
#include "../../../foundation/NativeLibs/DynamicFfi/kira_dynamic_ffi.h"

#include <stdint.h>
#include <stddef.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

#if defined(_WIN32)
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <dlfcn.h>
#endif

enum {
    KIRA_FFI_OK = 0,
    KIRA_FFI_NULL_SYMBOL = 1,
    KIRA_FFI_LIBFFI_UNAVAILABLE = 2,
    KIRA_FFI_UNSUPPORTED_TYPE = 3,
    KIRA_FFI_PREP_FAILED = 4,
    KIRA_FFI_TOO_MANY_ARGS = 5
};

enum {
    KIRA_FFI_TYPE_VOID = 0,
    KIRA_FFI_TYPE_I8 = 1,
    KIRA_FFI_TYPE_U8 = 2,
    KIRA_FFI_TYPE_I16 = 3,
    KIRA_FFI_TYPE_U16 = 4,
    KIRA_FFI_TYPE_I32 = 5,
    KIRA_FFI_TYPE_U32 = 6,
    KIRA_FFI_TYPE_I64 = 7,
    KIRA_FFI_TYPE_U64 = 8,
    KIRA_FFI_TYPE_F32 = 9,
    KIRA_FFI_TYPE_F64 = 10,
    KIRA_FFI_TYPE_PTR = 11
};

typedef unsigned int KiraFfiAbi;
typedef unsigned int KiraFfiStatus;
typedef struct { uintptr_t words[64]; } KiraFfiCifStorage;

typedef KiraFfiStatus (*KiraFfiPrepCifFn)(void *, KiraFfiAbi, unsigned int, void *, void **);
typedef void (*KiraFfiCallFn)(void *, void (*)(void), void *, void **);

typedef struct {
    uintptr_t library;
    KiraFfiPrepCifFn prep_cif;
    KiraFfiCallFn call;
    void *type_void;
    void *type_uint8;
    void *type_sint8;
    void *type_uint16;
    void *type_sint16;
    void *type_uint32;
    void *type_sint32;
    void *type_uint64;
    void *type_sint64;
    void *type_float;
    void *type_double;
    void *type_pointer;
} KiraLibffi;

typedef union {
    int8_t i8;
    uint8_t u8;
    int16_t i16;
    uint16_t u16;
    int32_t i32;
    uint32_t u32;
    int64_t i64;
    uint64_t u64;
    float f32;
    double f64;
    uintptr_t ptr;
} KiraFfiScalar;

static KiraLibffi kira_libffi = {0};
static int32_t kira_dynamic_ffi_last_error = KIRA_FFI_OK;

#if defined(_WIN32)
static uintptr_t kira_os_library_open(const char *name) {
    if (name == NULL || name[0] == '\0') return 0;
    HMODULE handle = LoadLibraryA(name);
    return (uintptr_t)handle;
}

static void *kira_os_library_symbol(uintptr_t library, const char *name) {
    if (library == 0 || name == NULL) return NULL;
    return (void *)GetProcAddress((HMODULE)library, name);
}

static void kira_os_library_close(uintptr_t library) {
    if (library != 0) FreeLibrary((HMODULE)library);
}
#else
static uintptr_t kira_os_library_open(const char *name) {
    if (name == NULL || name[0] == '\0') return 0;
    return (uintptr_t)dlopen(name, RTLD_NOW | RTLD_LOCAL);
}

static void *kira_os_library_symbol(uintptr_t library, const char *name) {
    if (library == 0 || name == NULL) return NULL;
    return dlsym((void *)library, name);
}

static void kira_os_library_close(uintptr_t library) {
    if (library != 0) dlclose((void *)library);
}
#endif

KIRA_BRIDGE_EXPORT void *kira_dynamic_library_open(const char *name) {
    return (void *)kira_os_library_open(name);
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_library_symbol(void *library, const char *name) {
    return kira_os_library_symbol((uintptr_t)library, name);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_library_close(void *library) {
    kira_os_library_close((uintptr_t)library);
}

KIRA_BRIDGE_EXPORT uint8_t kira_dynamic_ptr_is_null(void *ptr) {
    return ptr == NULL ? 1 : 0;
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_null_ptr(void) {
    return NULL;
}

KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_host_platform_code(void) {
#if defined(_WIN32)
    return 1;
#elif defined(__linux__)
    return 2;
#elif defined(__APPLE__)
    return 3;
#else
    return 0;
#endif
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_alloc(uint64_t size) {
    return calloc(1, size == 0 ? 1 : (size_t)size);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_free(void *ptr) {
    free(ptr);
}

KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_read_u32(void *ptr) {
    if (ptr == NULL) return 0;
    return *(const uint32_t *)ptr;
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_read_i32(void *ptr) {
    if (ptr == NULL) return 0;
    return *(const int32_t *)ptr;
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_read_ptr(void *ptr) {
    if (ptr == NULL) return NULL;
    return *(void *const *)ptr;
}

KIRA_BRIDGE_EXPORT uint8_t kira_dynamic_read_u8_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0;
    return *((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT uint16_t kira_dynamic_read_u16_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0;
    return *(const uint16_t *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_read_u32_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0;
    return *(const uint32_t *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_read_i32_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0;
    return *(const int32_t *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT uint64_t kira_dynamic_read_u64_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0;
    return *(const uint64_t *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT int64_t kira_dynamic_read_i64_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0;
    return *(const int64_t *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_read_ptr_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return NULL;
    return *(void *const *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT float kira_dynamic_read_f32_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0.0f;
    return *(const float *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT double kira_dynamic_read_f64_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return 0.0;
    return *(const double *)((const uint8_t *)ptr + offset);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_u32(void *ptr, uint32_t value) {
    if (ptr != NULL) *(uint32_t *)ptr = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_ptr(void *ptr, void *value) {
    if (ptr != NULL) *(void **)ptr = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_u8_at(void *ptr, uint64_t offset, uint8_t value) {
    if (ptr != NULL) *((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_u16_at(void *ptr, uint64_t offset, uint16_t value) {
    if (ptr != NULL) *(uint16_t *)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_u32_at(void *ptr, uint64_t offset, uint32_t value) {
    if (ptr != NULL) *(uint32_t *)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_u64_at(void *ptr, uint64_t offset, uint64_t value) {
    if (ptr != NULL) *(uint64_t *)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_i64_at(void *ptr, uint64_t offset, int64_t value) {
    if (ptr != NULL) *(int64_t *)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_ptr_at(void *ptr, uint64_t offset, void *value) {
    if (ptr != NULL) *(void **)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_f32_at(void *ptr, uint64_t offset, float value) {
    if (ptr != NULL) *(float *)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_write_f64_at(void *ptr, uint64_t offset, double value) {
    if (ptr != NULL) *(double *)((uint8_t *)ptr + offset) = value;
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_cstring_dup(const char *text) {
    if (text == NULL) return NULL;
    size_t len = strlen(text);
    char *copy = (char *)malloc(len + 1);
    if (copy == NULL) return NULL;
    memcpy(copy, text, len + 1);
    return copy;
}

KIRA_BRIDGE_EXPORT const char *kira_dynamic_cstring_at(void *ptr, uint64_t offset) {
    if (ptr == NULL) return "";
    return (const char *)ptr + offset;
}

static int kira_append_path(char *buffer, size_t buffer_len, const char *root, const char *suffix) {
    if (root == NULL || root[0] == '\0') return 0;
#if defined(_WIN32)
    return snprintf(buffer, buffer_len, "%s\\%s", root, suffix) > 0;
#else
    return snprintf(buffer, buffer_len, "%s/%s", root, suffix) > 0;
#endif
}

static uintptr_t kira_try_open_libffi_path(const char *path) {
    if (path == NULL || path[0] == '\0') return 0;
    return kira_os_library_open(path);
}

static uintptr_t kira_open_managed_libffi(void) {
    const char *explicit_path = getenv("KIRA_LIBFFI_PATH");
    uintptr_t handle = kira_try_open_libffi_path(explicit_path);
    if (handle != 0) return handle;

    const char *home = getenv("KIRA_LIBFFI_HOME");
    char path[1024];
#if defined(_WIN32)
    if (kira_append_path(path, sizeof(path), home, "lib\\libffi-8.dll")) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    if (kira_append_path(path, sizeof(path), home, "bin\\libffi-8.dll")) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    const char *profile = getenv("USERPROFILE");
    if (profile != NULL && snprintf(path, sizeof(path), "%s\\.kira\\toolchains\\libffi\\3.5.2\\x86_64-windows-msvc\\lib\\libffi-8.dll", profile) > 0) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    if (profile != NULL && snprintf(path, sizeof(path), "%s\\.kira\\toolchains\\libffi\\3.5.2\\x86_64-windows-msvc\\bin\\libffi-8.dll", profile) > 0) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    handle = kira_try_open_libffi_path("libffi-8.dll");
    if (handle != 0) return handle;
    return kira_try_open_libffi_path("libffi.dll");
#elif defined(__APPLE__)
    if (kira_append_path(path, sizeof(path), home, "lib/libffi.8.dylib")) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    const char *user_home = getenv("HOME");
    if (user_home != NULL && snprintf(path, sizeof(path), "%s/.kira/toolchains/libffi/3.5.2/aarch64-macos/lib/libffi.8.dylib", user_home) > 0) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    handle = kira_try_open_libffi_path("libffi.8.dylib");
    if (handle != 0) return handle;
    return kira_try_open_libffi_path("libffi.dylib");
#else
    if (kira_append_path(path, sizeof(path), home, "lib/libffi.so")) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    const char *user_home = getenv("HOME");
    if (user_home != NULL && snprintf(path, sizeof(path), "%s/.kira/toolchains/libffi/3.5.2/x86_64-linux-gnu/lib/libffi.so", user_home) > 0) {
        handle = kira_try_open_libffi_path(path);
        if (handle != 0) return handle;
    }
    handle = kira_try_open_libffi_path("libffi.so.8");
    if (handle != 0) return handle;
    return kira_try_open_libffi_path("libffi.so");
#endif
}

static int kira_dynamic_ffi_ensure_libffi(void) {
    if (kira_libffi.library != 0) return 1;
    uintptr_t library = kira_open_managed_libffi();
    if (library == 0) {
        kira_dynamic_ffi_last_error = KIRA_FFI_LIBFFI_UNAVAILABLE;
        return 0;
    }

    kira_libffi.library = library;
    kira_libffi.prep_cif = (KiraFfiPrepCifFn)kira_os_library_symbol(library, "ffi_prep_cif");
    kira_libffi.call = (KiraFfiCallFn)kira_os_library_symbol(library, "ffi_call");
    kira_libffi.type_void = kira_os_library_symbol(library, "ffi_type_void");
    kira_libffi.type_uint8 = kira_os_library_symbol(library, "ffi_type_uint8");
    kira_libffi.type_sint8 = kira_os_library_symbol(library, "ffi_type_sint8");
    kira_libffi.type_uint16 = kira_os_library_symbol(library, "ffi_type_uint16");
    kira_libffi.type_sint16 = kira_os_library_symbol(library, "ffi_type_sint16");
    kira_libffi.type_uint32 = kira_os_library_symbol(library, "ffi_type_uint32");
    kira_libffi.type_sint32 = kira_os_library_symbol(library, "ffi_type_sint32");
    kira_libffi.type_uint64 = kira_os_library_symbol(library, "ffi_type_uint64");
    kira_libffi.type_sint64 = kira_os_library_symbol(library, "ffi_type_sint64");
    kira_libffi.type_float = kira_os_library_symbol(library, "ffi_type_float");
    kira_libffi.type_double = kira_os_library_symbol(library, "ffi_type_double");
    kira_libffi.type_pointer = kira_os_library_symbol(library, "ffi_type_pointer");

    if (kira_libffi.prep_cif == NULL || kira_libffi.call == NULL || kira_libffi.type_pointer == NULL) {
        kira_os_library_close(library);
        memset(&kira_libffi, 0, sizeof(kira_libffi));
        kira_dynamic_ffi_last_error = KIRA_FFI_LIBFFI_UNAVAILABLE;
        return 0;
    }
    return 1;
}

static void *kira_ffi_type(uint32_t tag) {
    switch (tag) {
        case KIRA_FFI_TYPE_VOID: return kira_libffi.type_void;
        case KIRA_FFI_TYPE_I8: return kira_libffi.type_sint8;
        case KIRA_FFI_TYPE_U8: return kira_libffi.type_uint8;
        case KIRA_FFI_TYPE_I16: return kira_libffi.type_sint16;
        case KIRA_FFI_TYPE_U16: return kira_libffi.type_uint16;
        case KIRA_FFI_TYPE_I32: return kira_libffi.type_sint32;
        case KIRA_FFI_TYPE_U32: return kira_libffi.type_uint32;
        case KIRA_FFI_TYPE_I64: return kira_libffi.type_sint64;
        case KIRA_FFI_TYPE_U64: return kira_libffi.type_uint64;
        case KIRA_FFI_TYPE_F32: return kira_libffi.type_float;
        case KIRA_FFI_TYPE_F64: return kira_libffi.type_double;
        case KIRA_FFI_TYPE_PTR: return kira_libffi.type_pointer;
        default: return NULL;
    }
}

static KiraFfiAbi kira_ffi_system_abi(void) {
#if defined(_WIN32) && defined(_M_X64)
    return 1;
#elif defined(__x86_64__) || defined(_M_X64)
    return 2;
#else
    return 1;
#endif
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_last_error_code(void) {
    return kira_dynamic_ffi_last_error;
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_call(
    void *function_ptr,
    uint32_t result_type,
    const uint32_t *arg_types,
    const void *arg_values,
    uint32_t arg_count,
    void *result_out
) {
    if (function_ptr == NULL) return (kira_dynamic_ffi_last_error = KIRA_FFI_NULL_SYMBOL);
    if (arg_count > 32) return (kira_dynamic_ffi_last_error = KIRA_FFI_TOO_MANY_ARGS);
    if (!kira_dynamic_ffi_ensure_libffi()) return kira_dynamic_ffi_last_error;

    void *ffi_result_type = kira_ffi_type(result_type);
    if (ffi_result_type == NULL) return (kira_dynamic_ffi_last_error = KIRA_FFI_UNSUPPORTED_TYPE);

    const uintptr_t *arg_words = (const uintptr_t *)arg_values;
    void *ffi_arg_types[32];
    KiraFfiScalar storage[32];
    void *arg_ptrs[32];
    for (uint32_t i = 0; i < arg_count; i++) {
        ffi_arg_types[i] = kira_ffi_type(arg_types[i]);
        if (ffi_arg_types[i] == NULL) return (kira_dynamic_ffi_last_error = KIRA_FFI_UNSUPPORTED_TYPE);
        storage[i].u64 = arg_words[i];
        arg_ptrs[i] = &storage[i];
    }

    KiraFfiCifStorage cif;
    memset(&cif, 0, sizeof(cif));
    if (kira_libffi.prep_cif(&cif, kira_ffi_system_abi(), arg_count, ffi_result_type, ffi_arg_types) != 0) {
        return (kira_dynamic_ffi_last_error = KIRA_FFI_PREP_FAILED);
    }
    kira_libffi.call(&cif, (void (*)(void))function_ptr, result_out, arg_ptrs);
    kira_dynamic_ffi_last_error = KIRA_FFI_OK;
    return KIRA_FFI_OK;
}

enum { KIRA_DYNAMIC_CALL_MAX_ARGS = 32 };

typedef struct {
    uint32_t capacity;
    uint32_t count;
    uint32_t types[KIRA_DYNAMIC_CALL_MAX_ARGS];
    uintptr_t values[KIRA_DYNAMIC_CALL_MAX_ARGS];
} KiraDynamicCall;

KIRA_BRIDGE_EXPORT void *kira_dynamic_call_new(uint32_t max_args) {
    if (max_args > KIRA_DYNAMIC_CALL_MAX_ARGS) return NULL;
    KiraDynamicCall *call = (KiraDynamicCall *)calloc(1, sizeof(KiraDynamicCall));
    if (call == NULL) return NULL;
    call->capacity = max_args == 0 ? KIRA_DYNAMIC_CALL_MAX_ARGS : max_args;
    return call;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_reset(void *call) {
    if (call != NULL) ((KiraDynamicCall *)call)->count = 0;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_free(void *call) {
    free(call);
}

static void kira_dynamic_call_append(void *call_ptr, uint32_t type_tag, uintptr_t value) {
    KiraDynamicCall *call = (KiraDynamicCall *)call_ptr;
    if (call == NULL || call->count >= call->capacity) return;
    call->types[call->count] = type_tag;
    call->values[call->count] = value;
    call->count += 1;
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_ptr(void *call, void *value) {
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_PTR, (uintptr_t)value);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_i32(void *call, int32_t value) {
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_I32, (uintptr_t)(uint32_t)value);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_u32(void *call, uint32_t value) {
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_U32, (uintptr_t)value);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_i64(void *call, int64_t value) {
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_I64, (uintptr_t)(uint64_t)value);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_u64(void *call, uint64_t value) {
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_U64, (uintptr_t)value);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_f32(void *call, float value) {
    KiraFfiScalar scalar;
    scalar.u64 = 0;
    scalar.f32 = value;
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_F32, (uintptr_t)scalar.u64);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_arg_f64(void *call, double value) {
    KiraFfiScalar scalar;
    scalar.f64 = value;
    kira_dynamic_call_append(call, KIRA_FFI_TYPE_F64, (uintptr_t)scalar.u64);
}

static int32_t kira_dynamic_call_invoke(void *call_ptr, void *function_ptr, uint32_t result_type, KiraFfiScalar *result) {
    KiraDynamicCall *call = (KiraDynamicCall *)call_ptr;
    result->u64 = 0;
    if (call == NULL) return (kira_dynamic_ffi_last_error = KIRA_FFI_NULL_SYMBOL);
    return kira_dynamic_ffi_call(function_ptr, result_type, call->types, call->values, call->count, result);
}

KIRA_BRIDGE_EXPORT void kira_dynamic_call_invoke_void(void *call, void *function_ptr) {
    KiraFfiScalar result;
    (void)kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_VOID, &result);
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_call_invoke_i32(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_I32, &result) != KIRA_FFI_OK) return 0;
    return result.i32;
}

KIRA_BRIDGE_EXPORT uint32_t kira_dynamic_call_invoke_u32(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_U32, &result) != KIRA_FFI_OK) return 0;
    return result.u32;
}

KIRA_BRIDGE_EXPORT int64_t kira_dynamic_call_invoke_i64(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_I64, &result) != KIRA_FFI_OK) return 0;
    return result.i64;
}

KIRA_BRIDGE_EXPORT uint64_t kira_dynamic_call_invoke_u64(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_U64, &result) != KIRA_FFI_OK) return 0;
    return result.u64;
}

KIRA_BRIDGE_EXPORT void *kira_dynamic_call_invoke_ptr(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_PTR, &result) != KIRA_FFI_OK) return NULL;
    return (void *)result.ptr;
}

KIRA_BRIDGE_EXPORT float kira_dynamic_call_invoke_f32(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_F32, &result) != KIRA_FFI_OK) return 0.0f;
    return result.f32;
}

KIRA_BRIDGE_EXPORT double kira_dynamic_call_invoke_f64(void *call, void *function_ptr) {
    KiraFfiScalar result;
    if (kira_dynamic_call_invoke(call, function_ptr, KIRA_FFI_TYPE_F64, &result) != KIRA_FFI_OK) return 0.0;
    return result.f64;
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_call_i32_ptr(void *function_ptr, void *arg0) {
    uint32_t arg_types[1] = { KIRA_FFI_TYPE_PTR };
    uintptr_t arg_values[1] = { (uintptr_t)arg0 };
    int32_t result = 0;
    if (kira_dynamic_ffi_call(function_ptr, KIRA_FFI_TYPE_I32, arg_types, arg_values, 1, &result) != KIRA_FFI_OK) return 0;
    return result;
}

KIRA_BRIDGE_EXPORT int32_t kira_dynamic_ffi_call_i32_ptr_u32_ptr_ptr(
    void *function_ptr,
    void *arg0,
    uint32_t arg1,
    void *arg2,
    void *arg3
) {
    uint32_t arg_types[4] = { KIRA_FFI_TYPE_PTR, KIRA_FFI_TYPE_U32, KIRA_FFI_TYPE_PTR, KIRA_FFI_TYPE_PTR };
    uintptr_t arg_values[4] = { (uintptr_t)arg0, (uintptr_t)arg1, (uintptr_t)arg2, (uintptr_t)arg3 };
    int32_t result = 0;
    if (kira_dynamic_ffi_call(function_ptr, KIRA_FFI_TYPE_I32, arg_types, arg_values, 4, &result) != KIRA_FFI_OK) return 0;
    return result;
}
