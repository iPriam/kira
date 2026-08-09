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

/* Inline fixed-size array members: storage held inside the struct, not a
 * pointer to it. `ffi_grid` holds scalars, `ffi_board` holds structs — the two
 * shapes an `@FFI.Array` element can take. */
struct ffi_grid {
    int cells[4];
    double weight;
};
struct ffi_board {
    struct ffi_inner slots[3];
    int tag;
};

int ffi_grid_sum(struct ffi_grid g);
struct ffi_grid ffi_grid_make(int base);
int ffi_board_sum(struct ffi_board b);
struct ffi_board ffi_board_make(int seed);

/* A function-pointer member, and the two directions it travels: C hands one out,
 * Kira stores it in a struct, and C calls it back through that struct. A null
 * member is the zero-filled case and must be observable as such. */
typedef int (*ffi_adder)(int, int);
struct ffi_hooks {
    ffi_adder add;
    int scale;
};

ffi_adder ffi_default_adder(void);
/* Calls the adder it is handed twice and folds the results, so a Kira callback
 * has to survive more than one crossing and its arguments have to arrive in
 * order. */
int ffi_fold_adder(ffi_adder add, int a, int b);
/* Stores a callback and calls it later, which is the shape a native API with a
 * descriptor struct uses. */
void ffi_store_adder(struct ffi_hooks h);
int ffi_run_stored(int a, int b);
int ffi_hooks_apply(struct ffi_hooks h, int a, int b);
int ffi_call_adder(ffi_adder add, int a, int b);

/* A returned borrowed C string: storage the callee keeps and never hands over,
 * which the seam copies on the way out so Kira holds no C memory and frees
 * none. `ffi_echo_or_null` returns NULL for an empty input, which is the case
 * that has to read as the empty string rather than crash. */
const char *ffi_greeting(void);
const char *ffi_echo_or_null(const char *s);

/* A callback C enters with a `const char*` it owns. The Kira side has to see
 * owned text — a copy made while the pointer is still C's — and a NULL argument
 * has to arrive as the empty string rather than as a crash. */
typedef int (*ffi_labeler)(const char *text, int n);
int ffi_call_labeler(ffi_labeler label, const char *text, int n);
int ffi_call_labeler_null(ffi_labeler label, int n);

/* A descriptor with a `const char*` member, the shape a windowing library hands
 * over once and reads for the rest of the run. `ffi_desc_keep` stashes the
 * pointer instead of reading it, so `ffi_desc_recall` afterwards is what proves
 * the storage outlived the call rather than dangling. */
struct ffi_desc {
    int tag;
    const char *title;
};
int ffi_desc_by_value(struct ffi_desc d);
int ffi_desc_by_pointer(const struct ffi_desc *d);
void ffi_desc_keep(const struct ffi_desc *d);
int ffi_desc_recall(void);

/* An event struct C owns and hands over by address, the shape a windowing
 * library's callback argument has. Kira reads its members through the pointer
 * rather than calling an accessor per field, so the members are deliberately of
 * mixed width and signedness with padding between them: reading any of them at
 * the wrong offset gives a wrong answer rather than a crash. */
struct ffi_touch {
    unsigned long long identifier;
    float pos_x;
    float pos_y;
};
struct ffi_event {
    unsigned char kind;
    int code;
    float weight;
    signed char delta;
    struct ffi_touch touches[4];
    const struct ffi_event *next;
};
const struct ffi_event *ffi_event_current(void);
const struct ffi_event *ffi_event_none(void);

/* A buffer a caller builds and C reads, the shape every graphics API takes:
 * a pointer and a count. Kira hands over its own array; the seam writes the
 * elements out in C's widths. */
float ffi_sum_floats(const float *values, int count);
int ffi_sum_ints(const int *values, int count);

/* The same buffer, but named inside a descriptor rather than as two arguments —
 * `sg_range` and every other graphics API's data member. `ffi_range_keep`
 * stashes the pointer and `ffi_range_recall` sums it afterwards, which is what
 * proves the elements outlive the call that named them. */
struct ffi_range {
    const void *ptr;
    unsigned long long size;
};
float ffi_range_sum_floats(const struct ffi_range *range);
int ffi_range_sum_ints(struct ffi_range range);
void ffi_range_keep(const struct ffi_range *range);
int ffi_range_recall(void);

/* A C enum, which is an int with named values — the shape a graphics API's
 * every option takes. */
enum ffi_usage { FFI_USAGE_VERTEX = 0, FFI_USAGE_INDEX = 1, FFI_USAGE_UNIFORM = 2 };
int ffi_usage_stride(enum ffi_usage usage);

/* A generic instantiation on the Kira side arrives here as the struct it
 * instantiated to. */
struct ffi_pair {
    int first;
    int second;
};
int ffi_pair_sum(struct ffi_pair pair);

/* Writes through the buffer it is given, to show which direction an array
 * argument travels. */
void ffi_fill_floats(float *values, int count);

#endif /* KIRA_FFI_FIXTURE_H */
