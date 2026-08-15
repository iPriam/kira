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

int ffi_grid_sum(struct ffi_grid g) {
    int total = (int)g.weight;
    for (int i = 0; i < 4; i++) {
        total += g.cells[i];
    }
    return total;
}

struct ffi_grid ffi_grid_make(int base) {
    struct ffi_grid g;
    for (int i = 0; i < 4; i++) {
        g.cells[i] = base + i;
    }
    g.weight = base * 10;
    return g;
}

int ffi_board_sum(struct ffi_board b) {
    int total = b.tag;
    for (int i = 0; i < 3; i++) {
        total += b.slots[i].p * b.slots[i].q;
    }
    return total;
}

static int ffi_adder_impl(int a, int b) {
    return a + b;
}

ffi_adder ffi_default_adder(void) {
    return ffi_adder_impl;
}

int ffi_hooks_apply(struct ffi_hooks h, int a, int b) {
    if (h.add == 0) {
        return -1;
    }
    return h.add(a, b) * h.scale;
}

int ffi_fold_adder(ffi_adder add, int a, int b) {
    if (add == 0) {
        return -1;
    }
    return add(add(a, b), b);
}

static struct ffi_hooks ffi_stored_hooks;

void ffi_store_adder(struct ffi_hooks h) {
    ffi_stored_hooks = h;
}

int ffi_run_stored(int a, int b) {
    if (ffi_stored_hooks.add == 0) {
        return -1;
    }
    return ffi_stored_hooks.add(a, b) * ffi_stored_hooks.scale;
}

int ffi_call_adder(ffi_adder add, int a, int b) {
    if (add == 0) {
        return -1;
    }
    return add(a, b);
}

struct ffi_board ffi_board_make(int seed) {
    struct ffi_board b;
    for (int i = 0; i < 3; i++) {
        b.slots[i].p = seed + i;
        b.slots[i].q = 2;
    }
    b.tag = seed * 100;
    return b;
}

const char *ffi_greeting(void) {
    return "hello from C";
}

const char *ffi_echo_or_null(const char *s) {
    if (s == 0 || s[0] == 0) {
        return 0;
    }
    return s;
}

int ffi_call_labeler(ffi_labeler label, const char *text, int n) {
    if (label == 0) {
        return -1;
    }
    return label(text, n);
}

int ffi_call_labeler_null(ffi_labeler label, int n) {
    if (label == 0) {
        return -1;
    }
    return label(0, n);
}

/* 0 = the title was NULL, 1 = empty, 2 = "kira", 3 = anything else; combined
 * with the tag so one integer carries both facts. */
static int ffi_classify_title(const char *t) {
    if (t == 0) {
        return 0;
    }
    if (t[0] == 0) {
        return 1;
    }
    /* No libc here: the fixture is compiled without a sysroot, so the
       comparison is spelled out rather than borrowed from <string.h>. */
    const char *want = "kira";
    int i = 0;
    while (want[i] != 0 && t[i] == want[i]) {
        i++;
    }
    if (want[i] == 0 && t[i] == 0) {
        return 2;
    }
    return 3;
}

int ffi_desc_by_value(struct ffi_desc d) {
    return ffi_classify_title(d.title) * 10 + d.tag;
}

int ffi_desc_by_pointer(const struct ffi_desc *d) {
    if (d == 0) {
        return -1;
    }
    return ffi_classify_title(d->title) * 10 + d->tag;
}

static const char *ffi_kept_title;
static int ffi_kept_tag;

void ffi_desc_keep(const struct ffi_desc *d) {
    if (d == 0) {
        return;
    }
    ffi_kept_title = d->title;
    ffi_kept_tag = d->tag;
}

int ffi_desc_recall(void) {
    return ffi_classify_title(ffi_kept_title) * 10 + ffi_kept_tag;
}

static const struct ffi_event ffi_event_tail = {
    9, -70000, 0.25f, -3, {{0, 0.0f, 0.0f}}, 0,
};

static const struct ffi_event ffi_event_head = {
    200,
    -1234,
    1.5f,
    -7,
    {{1, 10.5f, 20.5f}, {2, 30.5f, 40.5f}, {3, 50.5f, 60.5f}, {4, 70.5f, 80.5f}},
    &ffi_event_tail,
};

const struct ffi_event *ffi_event_current(void) {
    return &ffi_event_head;
}

const struct ffi_event *ffi_event_none(void) {
    return 0;
}

float ffi_sum_floats(const float *values, int count) {
    float total = 0.0f;
    for (int i = 0; i < count; i += 1) {
        total += values[i];
    }
    return total;
}

int ffi_sum_ints(const int *values, int count) {
    int total = 0;
    for (int i = 0; i < count; i += 1) {
        total += values[i];
    }
    return total;
}

float ffi_range_sum_floats(const struct ffi_range *range) {
    if (range == 0 || range->ptr == 0) {
        return -1.0f;
    }
    return ffi_sum_floats((const float *)range->ptr,
                          (int)(range->size / sizeof(float)));
}

int ffi_range_sum_ints(struct ffi_range range) {
    if (range.ptr == 0) {
        return -1;
    }
    return ffi_sum_ints((const int *)range.ptr, (int)(range.size / sizeof(int)));
}

static struct ffi_range kept_range = {0, 0};

void ffi_range_keep(const struct ffi_range *range) {
    if (range != 0) {
        kept_range = *range;
    }
}

int ffi_range_recall(void) {
    return ffi_range_sum_ints(kept_range);
}

int ffi_usage_stride(enum ffi_usage usage) {
    switch (usage) {
        case FFI_USAGE_VERTEX: return 24;
        case FFI_USAGE_INDEX: return 4;
        case FFI_USAGE_UNIFORM: return 16;
    }
    return 0;
}

int ffi_pair_sum(struct ffi_pair pair) {
    return pair.first + pair.second;
}

void ffi_fill_floats(float *values, int count) {
    for (int i = 0; i < count; i += 1) {
        values[i] = 99.0f;
    }
}

int ffi_items_checksum(const struct ffi_item *items, int count) {
    if (items == 0) {
        return -1;
    }
    int total = 0;
    for (int i = 0; i < count; i += 1) {
        total += items[i].location * 1000 + (int)items[i].offset;
    }
    return total;
}

int ffi_item_list_checksum(const struct ffi_item_list *list) {
    if (list == 0) {
        return -1;
    }
    return ffi_items_checksum(list->items, list->count);
}

/* Walks the chain, scaling by every `ffi_chain_scale` link it finds. The cast
 * back from the link to the extension is the point: it is only sound because
 * the link is the extension's first member. */
int ffi_chained_total(const struct ffi_chained_descriptor *descriptor) {
    int total;
    const struct ffi_chain *link;
    if (descriptor == 0) {
        return -1;
    }
    total = descriptor->base;
    link = descriptor->next_in_chain;
    while (link != 0) {
        if (link->kind == 1) {
            total *= ((const struct ffi_chain_scale *)link)->factor;
        }
        link = link->next;
    }
    return total;
}

long long ffi_call_viewer(ffi_viewer view_cb, int tag, const char *data,
                          unsigned long long length) {
    struct ffi_view view;
    view.data = data;
    view.length = length;
    return view_cb(tag, view);
}

static ffi_viewer stored_viewer = 0;

void ffi_store_viewer(ffi_viewer view_cb) {
    stored_viewer = view_cb;
}

long long ffi_run_stored_viewer(int tag, const char *data, unsigned long long length) {
    if (stored_viewer == 0) {
        return -1;
    }
    return ffi_call_viewer(stored_viewer, tag, data, length);
}

double ffi_call_quad_taker(ffi_quad_taker take, double a, double b, double c, double d,
                           int tag) {
    struct ffi_quad q;
    q.a = a;
    q.b = b;
    q.c = c;
    q.d = d;
    return take(q, tag);
}

int ffi_call_userdata_twice(ffi_userdata_taker take, unsigned long long userdata, int n) {
    if (take == 0) {
        return -1;
    }
    /* Two crossings, so what the first one wrote through the userdata has to be
     * there when the second reads it. */
    int first = take(userdata, n);
    return take(userdata, first);
}
