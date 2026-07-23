// A thin C shim over GMP's mpf_t (arbitrary-precision float), giving
// CyborgPL's codegen simple, fixed function signatures to call directly
// (opaque void* handles, plain doubles/strings) instead of dealing with
// GMP's real C API (mpf_t's array-of-1-struct typedef trick, mp_bitcnt_t,
// mp_exp_t, etc.) from LLVM IR.
//
// A `bignum` handle is a malloc'd mpf_t, freed via bignum_free once
// codegen determines (via scope-exit/reassignment tracking) that nothing
// references it anymore. bignum_to_string's return value is still never
// freed -- a leaked but valid C string, safe to print -- since it's only
// ever handed straight to printf and CyborgPL has no other string
// lifetime story yet.

#include <gmp.h>
#include <stdlib.h>

void *bignum_new(unsigned long precision_bits) {
    mpf_t *x = malloc(sizeof(mpf_t));
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

    mpf_pow_ui(*(mpf_t *)dst, *(mpf_t *)base, exp_ui);

    if (mpf_sgn(e) < 0) {
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
    free(x);
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
