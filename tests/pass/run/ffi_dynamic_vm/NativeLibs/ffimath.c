#include "ffimath.h"
#include <string.h>

KIRA_FFIMATH_EXPORT int kira_ffi_add(int a, int b) {
    return a + b;
}

KIRA_FFIMATH_EXPORT double kira_ffi_scale(double x) {
    return x * 2.5;
}

KIRA_FFIMATH_EXPORT long long kira_ffi_strlen(const char *text) {
    return (long long)strlen(text);
}
