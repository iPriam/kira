#ifndef KIRA_FFIMATH_H
#define KIRA_FFIMATH_H

#if defined(_WIN32)
#define KIRA_FFIMATH_EXPORT __declspec(dllexport)
#else
#define KIRA_FFIMATH_EXPORT
#endif

KIRA_FFIMATH_EXPORT int kira_ffi_add(int a, int b);
KIRA_FFIMATH_EXPORT double kira_ffi_scale(double x);
KIRA_FFIMATH_EXPORT long long kira_ffi_strlen(const char *text);

#endif
