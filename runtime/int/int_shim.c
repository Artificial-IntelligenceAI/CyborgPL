// Backs `var:int`'s "loud failure over silent wrong data" guarantee.
// int is a genuine 64-bit integer -- unlike `num` (a float, which loses
// precision gracefully as values grow), overflowing int arithmetic would
// otherwise silently wrap around (two's-complement) to a wildly wrong
// value. Every case that can't be represented (an overflowing +/-/x/xx/
// xxx/!, a division by zero, negating the one value whose negation
// overflows) crashes with a clear message instead, the same "loud
// failure" precedent already used for an out-of-range array index, a
// failed file open, and invalid `input:num` text.
//
// One shared crash function, not one per case: every caller already
// knows exactly which message applies at the point it detects the
// problem, so there's nothing this function needs to figure out itself.

#include <stdio.h>
#include <stdlib.h>

void cyborg_int_die(const char *message) {
    fprintf(stderr, "%s\n", message);
    exit(1);
}
