// Backs print's/`overwrite`'s optional `[to*(dest)*]` file destination.
// codegen calls this instead of raw fopen so it never has to emit its own
// null-check/error IR -- same "crash with a clear message" philosophy
// already used for input:num's invalid input.

#include <stdio.h>
#include <stdlib.h>

FILE *cyborg_fopen_or_die(const char *path, const char *mode) {
    FILE *f = fopen(path, mode);
    if (f == NULL) {
        fprintf(stderr, "could not open '%s' for writing\n", path);
        exit(1);
    }
    return f;
}
