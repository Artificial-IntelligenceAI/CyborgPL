// A from-scratch, hand-written software implementation of IEEE-754
// binary128 ("quad precision") arithmetic.
//
// Why this exists: Apple's clang, on both Intel and Apple Silicon, defines
// `long double` as identical to `double` (64-bit) and does not implement
// GCC's `__float128` extension type either -- so there is no C-level type
// to build the *usual* compiler-rt quad-precision routines on top of for
// this target (their real source is guarded behind a check for exactly
// such a type, which is always false here -- confirmed by compiling their
// actual upstream source unmodified and finding the guarded function
// bodies compiled out entirely). `__uint128_t` (a 128-bit *integer*) is
// supported, though, so this file manually implements binary128's bit
// layout on top of that instead of relying on a native quad float type.
//
// Scope: only what CyborgPL's [precision:128] needs -- widening from
// 16/32/64-bit to 128-bit, narrowing back down, and add/sub/mul/div.
// No comparison routines (not needed yet).
//
// Simplification, stated up front rather than hidden: subnormal inputs
// and results are flushed to zero rather than fully preserved. Real
// subnormal handling needs its own careful normalization/denormalization
// logic on both ends; flush-to-zero is a standard, honest simplification
// used by plenty of real soft-float implementations, and CyborgPL programs
// are exceedingly unlikely to ever produce or notice numbers that small
// (subnormal doubles are around 1e-308 to 1e-324 in magnitude).

#include <stdint.h>

typedef __uint128_t u128;

// AAPCS64 passes a 128-bit *floating* type (which is what LLVM treats fp128
// as, for calling-convention purposes, even without hardware support) in a
// SIMD/FP register (Q0 etc.), not the general-purpose register pair that a
// plain __uint128_t argument would use. So the type actually exposed to the
// outside world at each of these function boundaries has to be a 128-bit
// vector type (confirmed by inspecting the generated assembly -- a
// __uint128_t-typed parameter here silently reads the wrong registers and
// produces garbage). All of the arithmetic below is written against plain
// u128 internally, exactly as before; only the external-facing wrapper
// functions at the bottom of this file convert at the boundary.
typedef unsigned long long v2u64 __attribute__((vector_size(16)));

static inline u128 u128_of_v2u64(v2u64 v) {
    u128 x;
    __builtin_memcpy(&x, &v, 16);
    return x;
}
static inline v2u64 v2u64_of_u128(u128 x) {
    v2u64 v;
    __builtin_memcpy(&v, &x, 16);
    return v;
}

#define F128_MANT_BITS 112
#define F128_EXP_BIAS 16383
#define F128_EXP_MAX 0x7FFF

#define F64_MANT_BITS 52
#define F64_EXP_BIAS 1023
#define F64_EXP_MAX 0x7FF

#define F32_MANT_BITS 23
#define F32_EXP_BIAS 127
#define F32_EXP_MAX 0xFF

#define F16_MANT_BITS 10
#define F16_EXP_BIAS 15
#define F16_EXP_MAX 0x1F

// ---- bit reinterpretation helpers (no aliasing violations: memcpy) ----

static inline uint64_t bits_of_f64(double d) {
    uint64_t u;
    __builtin_memcpy(&u, &d, 8);
    return u;
}
static inline double f64_of_bits(uint64_t u) {
    double d;
    __builtin_memcpy(&d, &u, 8);
    return d;
}
static inline uint32_t bits_of_f32(float f) {
    uint32_t u;
    __builtin_memcpy(&u, &f, 4);
    return u;
}
static inline float f32_of_bits(uint32_t u) {
    float f;
    __builtin_memcpy(&f, &u, 4);
    return f;
}
static inline uint16_t bits_of_f16(_Float16 h) {
    uint16_t u;
    __builtin_memcpy(&u, &h, 2);
    return u;
}
static inline _Float16 f16_of_bits(uint16_t u) {
    _Float16 h;
    __builtin_memcpy(&h, &u, 2);
    return h;
}

// Counts leading zeros across the full 128 bits -- __builtin_clzll only
// ever looks at 64 bits, so calling it directly on a u128 silently
// truncates to the low 64 bits and gives a wrong answer for anything with
// a set bit at position 64 or above.
static inline int clz128(u128 x) {
    uint64_t hi = (uint64_t)(x >> 64);
    if (hi != 0) {
        return __builtin_clzll(hi);
    }
    uint64_t lo = (uint64_t)x;
    if (lo != 0) {
        return 64 + __builtin_clzll(lo);
    }
    return 128;
}

// ================= extend: N-bit -> 128-bit (never needs rounding,
// since 128-bit always has strictly more mantissa bits available) =======

static u128 extenddftf2_impl(double a) {
    uint64_t bits = bits_of_f64(a);
    uint64_t sign = (bits >> 63) & 1;
    uint64_t exp = (bits >> F64_MANT_BITS) & F64_EXP_MAX;
    uint64_t mant = bits & (((uint64_t)1 << F64_MANT_BITS) - 1);
    u128 sign128 = (u128)sign << 127;

    if (exp == F64_EXP_MAX) { // infinity or NaN
        u128 mant128 = (u128)mant << (F128_MANT_BITS - F64_MANT_BITS);
        if (mant != 0 && mant128 == 0) mant128 = 1; // keep "is NaN"-ness alive
        return sign128 | ((u128)F128_EXP_MAX << F128_MANT_BITS) | mant128;
    }
    if (exp == 0) { // zero or subnormal double -- both flushed to signed zero
        return sign128;
    }
    int64_t new_exp = (int64_t)exp - F64_EXP_BIAS + F128_EXP_BIAS;
    u128 mant128 = (u128)mant << (F128_MANT_BITS - F64_MANT_BITS);
    return sign128 | ((u128)new_exp << F128_MANT_BITS) | mant128;
}

static u128 extendsftf2_impl(float a) {
    uint32_t bits = bits_of_f32(a);
    uint32_t sign = (bits >> 31) & 1;
    uint32_t exp = (bits >> F32_MANT_BITS) & F32_EXP_MAX;
    uint32_t mant = bits & (((uint32_t)1 << F32_MANT_BITS) - 1);
    u128 sign128 = (u128)sign << 127;

    if (exp == F32_EXP_MAX) {
        u128 mant128 = (u128)mant << (F128_MANT_BITS - F32_MANT_BITS);
        if (mant != 0 && mant128 == 0) mant128 = 1;
        return sign128 | ((u128)F128_EXP_MAX << F128_MANT_BITS) | mant128;
    }
    if (exp == 0) {
        return sign128;
    }
    int64_t new_exp = (int64_t)exp - F32_EXP_BIAS + F128_EXP_BIAS;
    u128 mant128 = (u128)mant << (F128_MANT_BITS - F32_MANT_BITS);
    return sign128 | ((u128)new_exp << F128_MANT_BITS) | mant128;
}

static u128 extendhftf2_impl(_Float16 a) {
    uint16_t bits = bits_of_f16(a);
    uint16_t sign = (bits >> 15) & 1;
    uint16_t exp = (bits >> F16_MANT_BITS) & F16_EXP_MAX;
    uint16_t mant = bits & (((uint16_t)1 << F16_MANT_BITS) - 1);
    u128 sign128 = (u128)sign << 127;

    if (exp == F16_EXP_MAX) {
        u128 mant128 = (u128)mant << (F128_MANT_BITS - F16_MANT_BITS);
        if (mant != 0 && mant128 == 0) mant128 = 1;
        return sign128 | ((u128)F128_EXP_MAX << F128_MANT_BITS) | mant128;
    }
    if (exp == 0) {
        return sign128;
    }
    int64_t new_exp = (int64_t)exp - F16_EXP_BIAS + F128_EXP_BIAS;
    u128 mant128 = (u128)mant << (F128_MANT_BITS - F16_MANT_BITS);
    return sign128 | ((u128)new_exp << F128_MANT_BITS) | mant128;
}

// ================= truncate: 128-bit -> N-bit (needs round-to-nearest-even,
// overflow -> infinity, underflow -> flushed to zero) =======

// Shared logic, parameterized by the target format's widths/bias. Returns
// the packed N-bit-wide result as a u128 (caller narrows to the real width).
static inline u128 trunc_from_128(u128 a, int dst_mant_bits, int64_t dst_exp_bias,
                                   int64_t dst_exp_max) {
    uint64_t sign = (uint64_t)(a >> 127) & 1;
    uint64_t exp = (uint64_t)(a >> F128_MANT_BITS) & F128_EXP_MAX;
    u128 mant = a & (((u128)1 << F128_MANT_BITS) - 1);
    u128 sign_dst = (u128)sign << 63; // placed as if into a 64-bit slot; caller shifts down as needed

    if (exp == F128_EXP_MAX) { // infinity or NaN
        int drop = F128_MANT_BITS - dst_mant_bits;
        uint64_t mant_dst = (uint64_t)(mant >> drop);
        if (mant != 0 && mant_dst == 0) mant_dst = 1;
        return sign_dst | ((u128)dst_exp_max << dst_mant_bits) | mant_dst;
    }
    if (exp == 0) { // zero or subnormal fp128 input -- flushed to signed zero
        return sign_dst;
    }

    int64_t unbiased = (int64_t)exp - F128_EXP_BIAS;
    int64_t new_exp = unbiased + dst_exp_bias;

    if (new_exp >= dst_exp_max) { // overflow -> infinity
        return sign_dst | ((u128)dst_exp_max << dst_mant_bits);
    }
    if (new_exp <= 0) { // underflow -> flushed to zero (no subnormal output)
        return sign_dst;
    }

    int drop_bits = F128_MANT_BITS - dst_mant_bits;
    uint64_t mant_dst = (uint64_t)(mant >> drop_bits);
    u128 remainder = mant & (((u128)1 << drop_bits) - 1);
    u128 halfway = (u128)1 << (drop_bits - 1);

    // round to nearest, ties to even
    if (remainder > halfway || (remainder == halfway && (mant_dst & 1))) {
        mant_dst += 1;
        if (mant_dst == ((uint64_t)1 << dst_mant_bits)) {
            mant_dst = 0;
            new_exp += 1;
            if (new_exp >= dst_exp_max) {
                return sign_dst | ((u128)dst_exp_max << dst_mant_bits);
            }
        }
    }

    return sign_dst | ((u128)new_exp << dst_mant_bits) | mant_dst;
}

static double trunctfdf2_impl(u128 a) {
    u128 packed = trunc_from_128(a, F64_MANT_BITS, F64_EXP_BIAS, F64_EXP_MAX);
    uint64_t sign = (uint64_t)(packed >> 63) & 1;
    uint64_t rest = (uint64_t)packed & (((uint64_t)1 << 63) - 1);
    return f64_of_bits((sign << 63) | rest);
}

static float trunctfsf2_impl(u128 a) {
    u128 packed = trunc_from_128(a, F32_MANT_BITS, F32_EXP_BIAS, F32_EXP_MAX);
    uint32_t sign = (uint32_t)(packed >> 63) & 1;
    uint32_t rest = (uint32_t)packed & (((uint32_t)1 << 31) - 1);
    return f32_of_bits((sign << 31) | rest);
}

static _Float16 trunctfhf2_impl(u128 a) {
    u128 packed = trunc_from_128(a, F16_MANT_BITS, F16_EXP_BIAS, F16_EXP_MAX);
    uint16_t sign = (uint16_t)(packed >> 63) & 1;
    uint16_t rest = (uint16_t)packed & (((uint16_t)1 << 15) - 1);
    return f16_of_bits((sign << 15) | rest);
}

// ================= arithmetic: add/sub/mul/div on two fp128 values =======

typedef struct {
    int sign;
    int is_special; // 1 if zero/inf/nan (mantissa/exp fields below unused)
    int is_inf;
    int is_nan;
    int is_zero;
    int64_t exp; // unbiased
    u128 mant;   // includes the implicit leading bit at position F128_MANT_BITS
} Decomposed;

static inline Decomposed decompose(u128 a) {
    Decomposed d;
    d.sign = (int)((a >> 127) & 1);
    uint64_t exp_field = (uint64_t)(a >> F128_MANT_BITS) & F128_EXP_MAX;
    u128 mant_field = a & (((u128)1 << F128_MANT_BITS) - 1);

    d.is_special = 0;
    d.is_inf = 0;
    d.is_nan = 0;
    d.is_zero = 0;

    if (exp_field == F128_EXP_MAX) {
        d.is_special = 1;
        if (mant_field == 0) d.is_inf = 1; else d.is_nan = 1;
        d.exp = 0;
        d.mant = 0;
        return d;
    }
    if (exp_field == 0) {
        // zero or subnormal (subnormal inputs flushed to zero, see file header)
        d.is_special = 1;
        d.is_zero = 1;
        d.exp = 0;
        d.mant = 0;
        return d;
    }
    d.exp = (int64_t)exp_field - F128_EXP_BIAS;
    d.mant = mant_field | ((u128)1 << F128_MANT_BITS); // restore implicit leading bit
    return d;
}

static inline u128 pack_normal(int sign, int64_t exp, u128 mant_with_implicit) {
    // mant_with_implicit has its leading bit at position F128_MANT_BITS (or
    // possibly one above it, if the caller just carried out of an addition;
    // normalize that one extra step here for convenience).
    if (mant_with_implicit >> (F128_MANT_BITS + 1)) {
        mant_with_implicit >>= 1;
        exp += 1;
    }
    // normalize leading zeros (e.g. after subtraction cancellation)
    while (mant_with_implicit != 0 && !(mant_with_implicit >> F128_MANT_BITS)) {
        mant_with_implicit <<= 1;
        exp -= 1;
    }
    if (mant_with_implicit == 0) {
        return (u128)sign << 127;
    }
    if (exp >= F128_EXP_MAX - F128_EXP_BIAS) {
        return ((u128)sign << 127) | ((u128)F128_EXP_MAX << F128_MANT_BITS); // overflow -> infinity
    }
    if (exp <= -F128_EXP_BIAS) {
        return (u128)sign << 127; // underflow -> zero (see file header)
    }
    u128 mant_field = mant_with_implicit & (((u128)1 << F128_MANT_BITS) - 1);
    uint64_t exp_field = (uint64_t)(exp + F128_EXP_BIAS);
    return ((u128)sign << 127) | ((u128)exp_field << F128_MANT_BITS) | mant_field;
}

static inline u128 make_nan(void) {
    return ((u128)F128_EXP_MAX << F128_MANT_BITS) | 1;
}
static inline u128 make_inf(int sign) {
    return ((u128)sign << 127) | ((u128)F128_EXP_MAX << F128_MANT_BITS);
}

static u128 addtf3_impl(u128 a, u128 b) {
    Decomposed da = decompose(a), db = decompose(b);

    if (da.is_nan || db.is_nan) return make_nan();
    if (da.is_inf && db.is_inf) {
        if (da.sign != db.sign) return make_nan(); // inf + -inf
        return make_inf(da.sign);
    }
    if (da.is_inf) return make_inf(da.sign);
    if (db.is_inf) return make_inf(db.sign);
    if (da.is_zero && db.is_zero) return (u128)(da.sign & db.sign) << 127;
    if (da.is_zero) return b;
    if (db.is_zero) return a;

    // align exponents: shift the smaller-magnitude-exponent operand right
    Decomposed *hi = &da, *lo = &db;
    if (db.exp > da.exp) { hi = &db; lo = &da; }
    int64_t shift = hi->exp - lo->exp;
    u128 lo_mant = (shift >= F128_MANT_BITS + 2) ? (lo->mant != 0 ? 1 : 0) : (lo->mant >> shift);
    // sticky bit: did we shift any 1-bits out?
    if (shift > 0 && shift < F128_MANT_BITS + 2) {
        u128 shifted_out = lo->mant & ((((u128)1 << shift) - 1));
        if (shifted_out != 0) lo_mant |= 1;
    }

    u128 result_mant;
    int result_sign;
    if (hi->sign == lo->sign) {
        result_mant = hi->mant + lo_mant;
        result_sign = hi->sign;
    } else if (hi->mant >= lo_mant) {
        result_mant = hi->mant - lo_mant;
        result_sign = hi->sign;
    } else {
        result_mant = lo_mant - hi->mant;
        result_sign = lo->sign;
    }
    // IEEE-754: exact cancellation (opposite signs summing to zero) is
    // always +0, never -0, except under round-toward-negative-infinity
    // (which we don't implement -- this codebase has no rounding-mode
    // concept at all).
    if (result_mant == 0) {
        result_sign = 0;
    }

    return pack_normal(result_sign, hi->exp, result_mant);
}

static u128 subtf3_impl(u128 a, u128 b) {
    // a - b == a + (-b)
    u128 neg_b = b ^ ((u128)1 << 127);
    return addtf3_impl(a, neg_b);
}

static u128 multf3_impl(u128 a, u128 b) {
    Decomposed da = decompose(a), db = decompose(b);
    int result_sign = da.sign ^ db.sign;

    if (da.is_nan || db.is_nan) return make_nan();
    if (da.is_inf || db.is_inf) {
        if (da.is_zero || db.is_zero) return make_nan(); // 0 * inf
        return make_inf(result_sign);
    }
    if (da.is_zero || db.is_zero) return (u128)result_sign << 127;

    // 113-bit mantissas (with implicit bit) multiplied give up to 226 bits;
    // split each into high/low 64-bit halves and do it via four 64x64->128
    // partial products, since __uint128_t * __uint128_t would silently
    // truncate to 128 bits and lose the high half we need.
    uint64_t a_hi = (uint64_t)(da.mant >> 64), a_lo = (uint64_t)da.mant;
    uint64_t b_hi = (uint64_t)(db.mant >> 64), b_lo = (uint64_t)db.mant;

    u128 lo_lo = (u128)a_lo * b_lo;
    u128 hi_lo = (u128)a_hi * b_lo;
    u128 lo_hi = (u128)a_lo * b_hi;
    u128 hi_hi = (u128)a_hi * b_hi;

    u128 mid = hi_lo + lo_hi + (lo_lo >> 64);
    u128 result_lo = (lo_lo & 0xFFFFFFFFFFFFFFFFULL) | (mid << 64);
    u128 result_hi = hi_hi + (mid >> 64);

    // product has 2*113 = 226 significant bits, occupying result_hi:result_lo
    // (a 256-bit conceptual value); the leading bit is around position 225 or
    // 226. We only need the top 114 bits (implicit bit + F128_MANT_BITS+1 for
    // rounding), everything below collapses into a sticky bit.

    // top_bit is the bit-index (0-based, within the 128+128-bit result_hi:result_lo
    // pair, counting result_lo as bits 0-127 and result_hi as bits 128-255)
    // of the product's leading 1. Since both inputs had their implicit bit at
    // F128_MANT_BITS (112), the product's leading bit is at 224 or 225.
    int top_bit = (result_hi != 0) ? (128 + (127 - clz128(result_hi)))
                                    : (127 - clz128(result_lo));
    int shift_needed = top_bit - (F128_MANT_BITS + 1); // how far right to shift to get 113 sig bits

    u128 mant113;
    int sticky = 0;
    if (shift_needed <= 0) {
        mant113 = result_lo << (-shift_needed);
    } else if (shift_needed < 128) {
        u128 shifted_out = result_lo & (((u128)1 << shift_needed) - 1);
        if (shifted_out != 0) sticky = 1;
        mant113 = (result_lo >> shift_needed) | (result_hi << (128 - shift_needed));
    } else {
        u128 shifted_out_lo_nonzero = (result_lo != 0);
        u128 shifted_out_hi = result_hi & (((u128)1 << (shift_needed - 128)) - 1);
        if (shifted_out_lo_nonzero || shifted_out_hi != 0) sticky = 1;
        mant113 = result_hi >> (shift_needed - 128);
    }
    // mant113 now has its leading bit at F128_MANT_BITS+1 (one extra bit for
    // rounding); round to nearest-even using that extra bit plus sticky.
    int round_bit = (int)(mant113 & 1);
    u128 mant_final = mant113 >> 1;
    if (round_bit && (sticky || (mant_final & 1))) {
        mant_final += 1;
    }

    // True value = (da.mant * db.mant) * 2^(da.exp+db.exp-2*F128_MANT_BITS),
    // and da.mant*db.mant == mant_final << (top_bit - F128_MANT_BITS)
    // (approx, modulo the rounding just applied), so folding that shift in:
    int64_t result_exp = da.exp + db.exp + top_bit - 2 * F128_MANT_BITS;
    return pack_normal(result_sign, result_exp, mant_final);
}

static u128 divtf3_impl(u128 a, u128 b) {
    Decomposed da = decompose(a), db = decompose(b);
    int result_sign = da.sign ^ db.sign;

    if (da.is_nan || db.is_nan) return make_nan();
    if (da.is_inf && db.is_inf) return make_nan();
    if (da.is_zero && db.is_zero) return make_nan();
    if (da.is_inf) return make_inf(result_sign);
    if (db.is_inf) return (u128)result_sign << 127;
    if (da.is_zero) return (u128)result_sign << 127;
    if (db.is_zero) return make_inf(result_sign);

    // Long division on the 113-bit mantissas, producing enough quotient
    // bits (113 + 2 guard bits) via restoring division, with everything
    // remaining collapsed into a sticky bit for round-to-nearest-even.
    u128 remainder = da.mant;
    u128 divisor = db.mant;
    u128 quotient = 0;
    int quotient_bits = F128_MANT_BITS + 3; // a couple extra for rounding

    // The bit-extraction loop below only produces a correct fractional
    // digit per iteration if remainder < divisor going in (each step at
    // most doubles remainder then subtracts divisor once). Since both
    // mantissas are normalized (leading bit at the same position), their
    // ratio is in (0.5, 2) -- so remainder can start >= divisor by at most
    // one factor of 2. Peel that off as an explicit leading bit first.
    if (remainder >= divisor) {
        remainder -= divisor;
        quotient = 1;
    }

    int sticky = 0;
    for (int i = 0; i < quotient_bits; i++) {
        remainder <<= 1;
        quotient <<= 1;
        if (remainder >= divisor) {
            remainder -= divisor;
            quotient |= 1;
        }
    }
    if (remainder != 0) sticky = 1;

    // quotient's leading 1 is at position quotient_bits-1 or quotient_bits-2
    // depending on whether da.mant >= db.mant to begin with; normalize.
    int top_bit = 127 - clz128(quotient);
    int shift_needed = top_bit - (F128_MANT_BITS + 1);

    u128 mant113;
    if (shift_needed <= 0) {
        mant113 = quotient << (-shift_needed);
    } else {
        u128 shifted_out = quotient & (((u128)1 << shift_needed) - 1);
        if (shifted_out != 0) sticky = 1;
        mant113 = quotient >> shift_needed;
    }

    int round_bit = (int)(mant113 & 1);
    u128 mant_final = mant113 >> 1;
    if (round_bit && (sticky || (mant_final & 1))) {
        mant_final += 1;
    }

    // True value = (da.mant/db.mant) * 2^(da.exp-db.exp), and quotient ==
    // (da.mant << quotient_bits)/db.mant == mant_final << (top_bit-F128_MANT_BITS)
    // (approx, modulo the rounding just applied), so folding that shift in:
    int64_t result_exp = da.exp - db.exp + top_bit - quotient_bits;
    return pack_normal(result_sign, result_exp, mant_final);
}

// ================= external ABI: the real symbol names LLVM calls, using
// v2u64 for the fp128 side so the argument/return lands in the SIMD/FP
// register AAPCS64 actually uses for it (see the v2u64 typedef comment
// above) =======

v2u64 __extenddftf2(double a) { return v2u64_of_u128(extenddftf2_impl(a)); }
v2u64 __extendsftf2(float a) { return v2u64_of_u128(extendsftf2_impl(a)); }
v2u64 __extendhftf2(_Float16 a) { return v2u64_of_u128(extendhftf2_impl(a)); }

double __trunctfdf2(v2u64 a) { return trunctfdf2_impl(u128_of_v2u64(a)); }
float __trunctfsf2(v2u64 a) { return trunctfsf2_impl(u128_of_v2u64(a)); }
_Float16 __trunctfhf2(v2u64 a) { return trunctfhf2_impl(u128_of_v2u64(a)); }

v2u64 __addtf3(v2u64 a, v2u64 b) {
    return v2u64_of_u128(addtf3_impl(u128_of_v2u64(a), u128_of_v2u64(b)));
}
v2u64 __subtf3(v2u64 a, v2u64 b) {
    return v2u64_of_u128(subtf3_impl(u128_of_v2u64(a), u128_of_v2u64(b)));
}
v2u64 __multf3(v2u64 a, v2u64 b) {
    return v2u64_of_u128(multf3_impl(u128_of_v2u64(a), u128_of_v2u64(b)));
}
v2u64 __divtf3(v2u64 a, v2u64 b) {
    return v2u64_of_u128(divtf3_impl(u128_of_v2u64(a), u128_of_v2u64(b)));
}
