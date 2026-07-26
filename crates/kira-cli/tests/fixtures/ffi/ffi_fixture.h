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

/* A single-member C handle struct, passed and returned by value. A struct whose
 * one member is a `unsigned int` shares that member's register ABI, so the Kira
 * side crosses it as its `U32` field: it reads the field out of an argument and
 * rebuilds the struct around a result. */
struct ffi_handle {
    unsigned int id;
};
struct ffi_handle ffi_make_handle(unsigned int id);
unsigned int ffi_handle_id(struct ffi_handle h);
struct ffi_handle ffi_bump_handle(struct ffi_handle h);


/* C-layout aggregates crossing by value. `ffi_quad` is four doubles — on
 * AArch64 a homogeneous float aggregate passed and returned in v0-v3, which is
 * the register case a `byval`/`sret` lowering could not reach. `ffi_outer`
 * nests a struct, and `ffi_mixed` pads between a byte and a double.
 *
 * Plain C spellings, like the rest of this fixture: no system headers, so the
 * same source compiles with the managed clang and with emcc. */
struct ffi_rect {
    double x;
    double y;
};
struct ffi_quad {
    double a;
    double b;
    double c;
    double d;
};
struct ffi_inner {
    int p;
    int q;
};
struct ffi_outer {
    struct ffi_inner inner;
    double w;
};
struct ffi_mixed {
    signed char tag;
    double value;
    unsigned int count;
};

double ffi_rect_sum(struct ffi_rect r);
struct ffi_rect ffi_rect_scale(struct ffi_rect r, double k);
double ffi_quad_sum(struct ffi_quad q);
struct ffi_quad ffi_quad_make(double a, double b, double c, double d);
int ffi_outer_sum(struct ffi_outer o);
struct ffi_outer ffi_outer_make(int p, int q, double w);
long long ffi_mixed_sum(struct ffi_mixed m);
struct ffi_mixed ffi_mixed_make(signed char t, double v, unsigned int c);

#endif /* KIRA_FFI_FIXTURE_H */
