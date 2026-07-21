/*
 * Portable C fixture for Kira's seamless C-FFI proof.
 *
 * One function per supported foreign type, plus a stateful counter used to prove
 * the hybrid single-copy rule. No system headers and no libc beyond a
 * hand-rolled string length, so the same source compiles with the managed clang
 * for the host and with emcc for wasm32-emscripten.
 *
 * Every function uses fixed-width C spellings so the width is part of the ABI
 * contract, exactly as the Kira `@FFI.Extern` signature names it.
 */
#ifndef KIRA_FFI_FIXTURE_H
#define KIRA_FFI_FIXTURE_H

/* Signed and unsigned integer widths: negation and identity, so a wrong
 * sign- or zero-extension at the boundary shows up as a wrong number. */
signed char ffi_neg_i8(signed char x);
unsigned char ffi_id_u8(unsigned char x);
short ffi_neg_i16(short x);
unsigned short ffi_id_u16(unsigned short x);
int ffi_add_i32(int a, int b);
unsigned int ffi_id_u32(unsigned int x);
long long ffi_add_i64(long long a, long long b);
unsigned long long ffi_id_u64(unsigned long long x);

/* C `_Bool` in and out. */
_Bool ffi_not(_Bool b);

/* IEEE-754 single and double. */
float ffi_add_f32(float a, float b);
double ffi_add_f64(double a, double b);

/* A borrowed NUL-terminated C string, measured without libc. */
unsigned long long ffi_strlen(const char *s);

/* Opaque pointer words: a non-null one and a null one, plus reading a word
 * back out, so a round trip and a null both stay just data. */
void *ffi_make_ptr(void);
void *ffi_null_ptr(void);
long long ffi_ptr_word(void *p);

/* A `void` parameter and a `void` return, observed through a static cell. */
void ffi_store(int v);
int ffi_load(void);

/* A process-wide counter: each call increments and returns it. Reached from a
 * runtime-half call and a native-half call, it must count 1 then 2 — proof that
 * both halves share one copy of this code rather than two. */
long long ffi_bump(void);

#endif /* KIRA_FFI_FIXTURE_H */
