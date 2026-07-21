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
