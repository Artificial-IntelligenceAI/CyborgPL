// Backs print's/`overwrite`'s `[to*(dest)*]` file destination and
// `input:`'s `[from*(dest)*]` file source. codegen calls these instead of
// raw fopen/fread so it never has to emit its own null-check/error IR --
// same "crash with a clear message" philosophy already used for
// input:num's invalid input.

#include <stdio.h>
#include <stdlib.h>

FILE *cyborg_fopen_or_die(const char *path, const char *mode) {
    FILE *f = fopen(path, mode);
    if (f == NULL) {
        fprintf(stderr, "could not open '%s' (mode '%s')\n", path, mode);
        exit(1);
    }
    return f;
}

// Reads an entire file's content into a fresh, owned, null-terminated
// buffer -- adopted directly as a `str` value by codegen, same as
// cyborg_read_line's stdin equivalent (no extra copy needed).
char *cyborg_read_file_or_die(const char *path) {
    FILE *f = cyborg_fopen_or_die(path, "r");
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    fseek(f, 0, SEEK_SET);
    char *buf = malloc((size_t)size + 1);
    size_t read = fread(buf, 1, (size_t)size, f);
    buf[read] = '\0';
    fclose(f);
    return buf;
}
