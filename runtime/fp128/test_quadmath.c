#include <stdio.h>
#include <math.h>
#include <string.h>

typedef unsigned long long v2u64 __attribute__((vector_size(16)));

extern v2u64 __extenddftf2(double a);
extern double __trunctfdf2(v2u64 a);
extern v2u64 __addtf3(v2u64 a, v2u64 b);
extern v2u64 __subtf3(v2u64 a, v2u64 b);
extern v2u64 __multf3(v2u64 a, v2u64 b);
extern v2u64 __divtf3(v2u64 a, v2u64 b);

static int failures = 0;
static int total = 0;

static int same_double(double a, double b) {
    if (isnan(a) && isnan(b)) return 1;
    return memcmp(&a, &b, sizeof(double)) == 0; // exact bit match (distinguishes +/-0 too)
}

static void check_roundtrip(double a) {
    total++;
    double back = __trunctfdf2(__extenddftf2(a));
    if (!same_double(a, back)) {
        printf("FAIL roundtrip: %.17g -> %.17g\n", a, back);
        failures++;
    }
}

static void check_op(const char *name, double a, double b, v2u64 (*op)(v2u64, v2u64), double expected) {
    total++;
    v2u64 ua = __extenddftf2(a), ub = __extenddftf2(b);
    v2u64 ur = op(ua, ub);
    double got = __trunctfdf2(ur);
    if (!same_double(got, expected)) {
        printf("FAIL %s(%.17g, %.17g) = %.17g, expected %.17g\n", name, a, b, got, expected);
        failures++;
    }
}

static void check_add(double a, double b) { check_op("add", a, b, __addtf3, a + b); }
static void check_sub(double a, double b) { check_op("sub", a, b, __subtf3, a - b); }
static void check_mul(double a, double b) { check_op("mul", a, b, __multf3, a * b); }
static void check_div(double a, double b) { check_op("div", a, b, __divtf3, a / b); }

int main() {
    // --- round-trip (extend then truncate) ---
    double roundtrip_vals[] = {
        0.0, -0.0, 1.0, -1.0, 3.14159265358979, -3.14159265358979,
        2.5, 0.1, 100.0, 1e10, 1e-10, 1e300, 1e-300, 5.0,
        1.0/3.0, 123456789.123456789, -0.0001, 9007199254740993.0,
        INFINITY, -INFINITY, NAN,
    };
    for (size_t i = 0; i < sizeof(roundtrip_vals)/sizeof(roundtrip_vals[0]); i++) {
        check_roundtrip(roundtrip_vals[i]);
    }

    // --- arithmetic: a broad mix of values ---
    double vals[] = {
        0.0, -0.0, 1.0, -1.0, 2.0, -2.0, 2.5, -2.5, 0.5, 5.0, 60.0,
        3.14159265358979, 100.0, 0.1, 0.2, 1e10, 1e-10, 1e100, 1e-100,
        123.456, -123.456, 7.0, 3.0, 1.5, 74.0,
    };
    size_t n = sizeof(vals)/sizeof(vals[0]);
    for (size_t i = 0; i < n; i++) {
        for (size_t j = 0; j < n; j++) {
            check_add(vals[i], vals[j]);
            check_sub(vals[i], vals[j]);
            check_mul(vals[i], vals[j]);
            if (vals[j] != 0.0) check_div(vals[i], vals[j]);
        }
    }

    // --- special values ---
    check_add(INFINITY, 1.0);
    check_add(INFINITY, -INFINITY);
    check_add(0.0, -0.0);
    check_mul(INFINITY, 0.0);
    check_mul(2.0, INFINITY);
    check_div(1.0, INFINITY);
    check_div(1.0, 0.0);
    check_div(0.0, 0.0);

    printf("%d / %d checks passed\n", total - failures, total);
    return failures != 0;
}
