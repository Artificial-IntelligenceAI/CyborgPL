// A thin C shim over GMP's mpz_t (arbitrary-precision integer) -- the
// counterpart to bignum_shim.c's mpf_t (arbitrary-precision decimal).
// Same opaque-handle convention: a `bigint` handle is a malloc'd mpz_t,
// freed via bigint_free once codegen's scope tracking determines nothing
// references it anymore. No precision/width parameter anywhere here --
// unlike bignum (which needs a bit count to track decimal precision) or
// int (a fixed hardware width), bigint just grows to hold whatever value
// it's given.
//
// Reuses the exact same mimalloc-backed GMP allocator hook
// bignum_shim.c installs (mp_set_memory_functions is process-wide, not
// per-type -- there's nothing to install twice here). Both this file and
// bignum_shim.c are always linked into every compiled program regardless
// of which types it actually uses, so whichever one's constructor runs
// first, the hook is already in place before either bignum or bigint
// ever allocates.

#include <gmp.h>
#include <mimalloc.h>
#include <stdlib.h>

// runtime/int/int_shim.c -- the same shared crash-with-message path
// int's own overflow/division-by-zero checks already use.
extern void cyborg_int_die(const char *message);

void *bigint_new(void) {
    mpz_t *x = mi_malloc(sizeof(mpz_t));
    mpz_init(*x);
    return x;
}

void bigint_set_str(void *x, const char *s) {
    // Base 10, matching bignum_set_str; the parser/type checker already
    // guarantee this is a whole number's digit text (an optional leading
    // '-', digits only), never anything GMP's own base-10 parser would
    // reject.
    mpz_set_str(*(mpz_t *)x, s, 10);
}

void bigint_copy(void *dst, void *src) {
    mpz_set(*(mpz_t *)dst, *(mpz_t *)src);
}

void bigint_add(void *dst, void *a, void *b) {
    mpz_add(*(mpz_t *)dst, *(mpz_t *)a, *(mpz_t *)b);
}

void bigint_sub(void *dst, void *a, void *b) {
    mpz_sub(*(mpz_t *)dst, *(mpz_t *)a, *(mpz_t *)b);
}

void bigint_mul(void *dst, void *a, void *b) {
    mpz_mul(*(mpz_t *)dst, *(mpz_t *)a, *(mpz_t *)b);
}

// Truncating division (toward zero), matching `int`'s own `/`. Crashes
// on division by zero via the same shared message path `int` uses,
// rather than relying on GMP's own (undefined) behavior for a zero
// divisor.
void bigint_div(void *dst, void *a, void *b) {
    if (mpz_sgn(*(mpz_t *)b) == 0) {
        cyborg_int_die("bigint division by zero");
    }
    mpz_tdiv_q(*(mpz_t *)dst, *(mpz_t *)a, *(mpz_t *)b);
}

void bigint_neg(void *dst, void *src) {
    mpz_neg(*(mpz_t *)dst, *(mpz_t *)src);
}

// mpz_cmp's own convention (negative/zero/positive) is exactly what
// codegen needs -- it just compares the result against 0 with whichever
// predicate the source actually asked for.
int bigint_cmp(void *a, void *b) {
    return mpz_cmp(*(mpz_t *)a, *(mpz_t *)b);
}

// GMP's mpz_pow_ui only takes a non-negative `unsigned long` exponent --
// a negative exponent would only ever produce a fraction, which bigint
// (whole numbers only) can't represent, so that's a hard error, the same
// "loud failure" int's own `xx` already gives a negative exponent.
void bigint_pow(void *dst, void *base, void *exp) {
    if (mpz_sgn(*(mpz_t *)exp) < 0) {
        cyborg_int_die("bigint power requires a non-negative exponent");
    }
    unsigned long exp_ui = mpz_get_ui(*(mpz_t *)exp);
    mpz_pow_ui(*(mpz_t *)dst, *(mpz_t *)base, exp_ui);
}

// `xxx`: height copies of base (a xxx b = a^(a^(a^...))), height only
// known at runtime so this is a real loop -- done here in C rather than
// as hand-built LLVM IR the way bignum's tetration is, since there's no
// per-iteration allocation to avoid: `dst` is a single handle mutated in
// place throughout, and mpz_pow_ui's exponent is a plain unsigned long
// (not another mpz_t), so there's no aliasing concern to reason about
// the way bignum_pow's mpf_t exponent needed. `height` is a bigint
// handle like every other operand (read down to a native unsigned long
// once, here, rather than codegen doing that conversion itself).
void bigint_tetration(void *dst, void *base, void *height) {
    unsigned long height_ui = mpz_get_ui(*(mpz_t *)height);
    mpz_set(*(mpz_t *)dst, *(mpz_t *)base);
    for (unsigned long i = 2; i <= height_ui; i++) {
        unsigned long exp_ui = mpz_get_ui(*(mpz_t *)dst);
        mpz_pow_ui(*(mpz_t *)dst, *(mpz_t *)base, exp_ui);
    }
}

// Postfix `!`: GMP's own mpz_fac_ui computes n! directly, far better
// optimized than a hand-rolled multiplication loop. `n` is read down to
// a native unsigned long first -- a negative n silently produces 1
// rather than erroring, consistent with int's own factorial (its loop
// simply never runs for n < 2), not stricter.
void bigint_factorial(void *dst, void *n) {
    if (mpz_sgn(*(mpz_t *)n) < 0) {
        mpz_set_ui(*(mpz_t *)dst, 1);
        return;
    }
    unsigned long n_ui = mpz_get_ui(*(mpz_t *)n);
    mpz_fac_ui(*(mpz_t *)dst, n_ui);
}

void bigint_free(void *x) {
    mpz_clear(*(mpz_t *)x);
    mi_free(x);
}

char *bigint_to_string(void *x) {
    // GMP's own decimal-string conversion -- NULL asks it to allocate
    // the buffer itself (sized to fit exactly), via whichever allocator
    // mp_set_memory_functions installed (mimalloc, same as bignum's own
    // buffers), freed by codegen the same way bignum_to_string's result
    // already is.
    return mpz_get_str(NULL, 10, *(mpz_t *)x);
}
