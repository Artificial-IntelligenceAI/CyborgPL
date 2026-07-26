// A thin C shim over GMP's mpf_t (arbitrary-precision float), giving
// CyborgPL's codegen simple, fixed function signatures to call directly
// (opaque void* handles, plain doubles/strings) instead of dealing with
// GMP's real C API (mpf_t's array-of-1-struct typedef trick, mp_bitcnt_t,
// mp_exp_t, etc.) from LLVM IR.
//
// A `bignum` handle is a malloc'd mpf_t, freed via bignum_free once
// codegen determines (via scope-exit/reassignment tracking) that nothing
// references it anymore. bignum_to_string's return value is a plain
// malloc'd C string, freed by codegen (a normal libc free()) once whatever
// consumed it (printf, or `stch`) is done reading it.

#include <gmp.h>
#include <stdlib.h>

// Every `bignum` value costs *two* heap allocations at construction: the
// `mpf_t` wrapper malloc'd below, and GMP's own internal limb-buffer
// allocation inside `mpf_init2`. Both get freed and immediately
// reallocated at the same size constantly (any repeated construct/use/
// free cycle at a given precision), so a size-bucketed freelist pool
// -- reusing an already-freed block of the right size instead of asking
// the system allocator again -- cuts that cost dramatically. Measured
// standalone (not a guess) before implementing: ~5-8x faster than plain
// malloc/free for a tight construct-use-free loop, landing within ~1-2x
// of the theoretical floor (doing no allocation at all). A pooled block
// is never returned to the OS once freed -- it stays reserved for reuse
// at that same size for the rest of the program's life, the one real
// tradeoff versus plain malloc/free (a program with a brief burst of
// many live bignums, then very few, keeps that memory reserved
// afterward instead of giving it back).
//
// Buckets are found by a linear scan keyed on requested size -- correct
// for any number of distinct precisions a program uses, and fast enough
// in practice since that count is always small (bounded by how many
// distinct `[precision:N]` values appear in the source, not by how many
// bignums are ever constructed).
typedef struct pool_bucket {
    size_t size;
    void *freelist; // each free block's own first word points to the next
    struct pool_bucket *next_bucket;
} pool_bucket;

static pool_bucket *pool_buckets = NULL;

static pool_bucket *pool_bucket_for(size_t size) {
    for (pool_bucket *b = pool_buckets; b; b = b->next_bucket) {
        if (b->size == size) {
            return b;
        }
    }
    pool_bucket *b = malloc(sizeof(pool_bucket));
    b->size = size;
    b->freelist = NULL;
    b->next_bucket = pool_buckets;
    pool_buckets = b;
    return b;
}

static void *pool_alloc(size_t size) {
    pool_bucket *b = pool_bucket_for(size);
    if (b->freelist) {
        void *block = b->freelist;
        b->freelist = *(void **)block;
        return block;
    }
    return malloc(size);
}

// GMP only ever calls this to grow a buffer within one still-live
// allocation (never observed in practice for a fixed-precision `mpf_t`
// whose size never changes after `mpf_init2` -- nothing here calls
// `mpf_set_prec`) -- not a hot path, so it isn't pooled, just delegated
// to the system allocator directly.
static void *pool_realloc(void *ptr, size_t old_size, size_t new_size) {
    (void)old_size;
    return realloc(ptr, new_size);
}

static void pool_free(void *ptr, size_t size) {
    pool_bucket *b = pool_bucket_for(size);
    *(void **)ptr = b->freelist;
    b->freelist = ptr;
}

// Installed before `main` even starts (a constructor, not called from
// anywhere in codegen) so every GMP allocation for the whole program's
// lifetime -- including the very first `bignum_new` -- goes through the
// pool. CyborgPL programs are single-threaded (no concurrency
// primitives exist in the language), so the unsynchronized freelists
// above are safe.
__attribute__((constructor))
static void install_bignum_pool(void) {
    mp_set_memory_functions(pool_alloc, pool_realloc, pool_free);
}

void *bignum_new(unsigned long precision_bits) {
    mpf_t *x = pool_alloc(sizeof(mpf_t));
    mpf_init2(*x, precision_bits);
    return x;
}

void bignum_set_d(void *x, double v) {
    mpf_set_d(*(mpf_t *)x, v);
}

void bignum_set_str(void *x, const char *s) {
    mpf_set_str(*(mpf_t *)x, s, 10);
}

void bignum_copy(void *dst, void *src) {
    mpf_set(*(mpf_t *)dst, *(mpf_t *)src);
}

void bignum_add(void *dst, void *a, void *b) {
    mpf_add(*(mpf_t *)dst, *(mpf_t *)a, *(mpf_t *)b);
}

void bignum_sub(void *dst, void *a, void *b) {
    mpf_sub(*(mpf_t *)dst, *(mpf_t *)a, *(mpf_t *)b);
}

void bignum_mul(void *dst, void *a, void *b) {
    mpf_mul(*(mpf_t *)dst, *(mpf_t *)a, *(mpf_t *)b);
}

void bignum_div(void *dst, void *a, void *b) {
    mpf_div(*(mpf_t *)dst, *(mpf_t *)a, *(mpf_t *)b);
}

void bignum_neg(void *dst, void *src) {
    mpf_neg(*(mpf_t *)dst, *(mpf_t *)src);
}

// mpf_cmp's own return convention (negative/zero/positive) is exactly
// what codegen needs -- it just compares the result against 0 with
// whichever predicate (<, ==, etc.) the source actually asked for.
int bignum_cmp(void *a, void *b) {
    return mpf_cmp(*(mpf_t *)a, *(mpf_t *)b);
}

// GMP's mpf_t has no general pow(): mpf_pow_ui only takes a non-negative
// integer exponent (its own real limitation, not a shortcut taken here).
// Negative integer exponents are handled via reciprocal (base^-n = 1/base^n);
// a fractional exponent's fractional part is silently truncated, same as
// how num's own tetration height is truncated to an integer already.
void bignum_pow(void *dst, void *base, void *exp) {
    mpf_srcptr e = *(mpf_t *)exp;
    mpf_t abs_e;
    mpf_init(abs_e);
    mpf_abs(abs_e, e);
    unsigned long exp_ui = mpf_get_ui(abs_e);
    mpf_clear(abs_e);
    // Captured before mpf_pow_ui runs: dst may alias exp (codegen reuses
    // a chained xx expression's own exponent handle as its destination),
    // in which case `e` -- a pointer into the same memory as exp, not a
    // copy -- would otherwise reflect the just-computed *result* by the
    // time it's read below, not the original exponent's sign.
    int exp_sign = mpf_sgn(e);

    mpf_pow_ui(*(mpf_t *)dst, *(mpf_t *)base, exp_ui);

    if (exp_sign < 0) {
        mpf_t one;
        mpf_init2(one, mpf_get_prec(*(mpf_t *)dst));
        mpf_set_ui(one, 1);
        mpf_div(*(mpf_t *)dst, one, *(mpf_t *)dst);
        mpf_clear(one);
    }
}

// Truncates to a native signed integer -- used to turn a bignum tetration
// height into a loop trip count, the same role build_float_to_signed_int
// plays for num's tetration.
long bignum_get_i64(void *x) {
    return mpf_get_si(*(mpf_t *)x);
}

void bignum_free(void *x) {
    mpf_clear(*(mpf_t *)x);
    pool_free(x, sizeof(mpf_t));
}

char *bignum_to_string(void *x) {
    mpf_srcptr v = (mpf_srcptr) * (mpf_t *)x;
    // Enough decimal digits to faithfully round-trip this value's own
    // precision (log10(2) =~ 0.30103 decimal digits per bit), plus a
    // couple extra for safety.
    int digits = (int)(mpf_get_prec(v) * 0.30103) + 2;
    char *out = NULL;
    gmp_asprintf(&out, "%.*Ff", digits, v);
    return out;
}
