// A thin C shim over GMP's mpf_t (arbitrary-precision float), giving
// CyborgPL's codegen simple, fixed function signatures to call directly
// (opaque void* handles, plain doubles/strings) instead of dealing with
// GMP's real C API (mpf_t's array-of-1-struct typedef trick, mp_bitcnt_t,
// mp_exp_t, etc.) from LLVM IR.
//
// A `bignum` handle is just a malloc'd mpf_t. Never freed -- the same
// accepted simplification already used elsewhere in this codebase (str
// literals, stch's old concat buffer): CyborgPL has no memory management
// story yet at all, and adding one just for bignum specifically wouldn't
// fix that. bignum_to_string's return value is the same story -- a leaked
// but valid C string, safe to print and never freed.

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
