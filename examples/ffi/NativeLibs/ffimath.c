/*
 * A tiny C library for the Kira seamless-FFI example.
 *
 * No system headers, so the same source builds for the host (managed clang) and
 * for wasm32-emscripten (emcc). See README.md for how to build the archive.
 */

int ffi_add(int a, int b) { return a + b; }

unsigned long long ffi_name_len(const char *s) {
    unsigned long long n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

void *ffi_origin(void) { return (void *)0; }

long long ffi_ptr_word(void *p) { return (long long)p; }
