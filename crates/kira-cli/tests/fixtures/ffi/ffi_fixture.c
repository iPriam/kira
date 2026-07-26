/*
 * Definitions for the Kira C-FFI fixture. See ffi_fixture.h for the contract.
 *
 * Deliberately free of system headers so it compiles unchanged for the host
 * (managed clang) and for wasm32-emscripten (emcc): the only libc-shaped thing
 * here, string length, is spelled out by hand.
 */
#include "ffi_fixture.h"

signed char ffi_neg_i8(signed char x) { return (signed char)(-x); }
unsigned char ffi_id_u8(unsigned char x) { return x; }
short ffi_neg_i16(short x) { return (short)(-x); }
unsigned short ffi_id_u16(unsigned short x) { return x; }
int ffi_add_i32(int a, int b) { return a + b; }
unsigned int ffi_id_u32(unsigned int x) { return x; }
long long ffi_add_i64(long long a, long long b) { return a + b; }
unsigned long long ffi_id_u64(unsigned long long x) { return x; }

_Bool ffi_not(_Bool b) { return !b; }

float ffi_add_f32(float a, float b) { return a + b; }
double ffi_add_f64(double a, double b) { return a + b; }

unsigned long long ffi_strlen(const char *s) {
    unsigned long long n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

void *ffi_make_ptr(void) { return (void *)42; }
void *ffi_null_ptr(void) { return (void *)0; }
long long ffi_ptr_word(void *p) { return (long long)p; }

static int ffi_cell = 0;
void ffi_store(int v) { ffi_cell = v; }
int ffi_load(void) { return ffi_cell; }

static long long ffi_counter = 0;
long long ffi_bump(void) { return ++ffi_counter; }

struct ffi_handle ffi_make_handle(unsigned int id) {
    struct ffi_handle h;
    h.id = id;
    return h;
}
unsigned int ffi_handle_id(struct ffi_handle h) { return h.id; }
struct ffi_handle ffi_bump_handle(struct ffi_handle h) {
    struct ffi_handle r;
    r.id = h.id + 1;
    return r;
}

double ffi_rect_sum(struct ffi_rect r) {
    return r.x + r.y;
}

struct ffi_rect ffi_rect_scale(struct ffi_rect r, double k) {
    struct ffi_rect out;
    out.x = r.x * k;
    out.y = r.y * k;
    return out;
}

double ffi_quad_sum(struct ffi_quad q) {
    return q.a + q.b + q.c + q.d;
}

struct ffi_quad ffi_quad_make(double a, double b, double c, double d) {
    struct ffi_quad q;
    q.a = a;
    q.b = b;
    q.c = c;
    q.d = d;
    return q;
}

int ffi_outer_sum(struct ffi_outer o) {
    return o.inner.p + o.inner.q + (int)o.w;
}

struct ffi_outer ffi_outer_make(int p, int q, double w) {
    struct ffi_outer o;
    o.inner.p = p;
    o.inner.q = q;
    o.w = w;
    return o;
}

long long ffi_mixed_sum(struct ffi_mixed m) {
    return (long long)m.tag + (long long)m.value + (long long)m.count;
}

struct ffi_mixed ffi_mixed_make(signed char t, double v, unsigned int c) {
    struct ffi_mixed m;
    m.tag = t;
    m.value = v;
    m.count = c;
    return m;
}
