// Backs `input:str`/`input:num`, reading a line from stdin.
//
// getline() already hands back a malloc'd buffer -- exactly the kind of
// owned heap buffer CyborgPL's `str` already expects every stored value to
// be (see codegen.rs's coerce_to_type), so cyborg_read_line's result is
// adopted directly as a `str` value with no extra copy needed.

#include <stdio.h>
#include <stdlib.h>

char *cyborg_read_line(void) {
    char *line = NULL;
    size_t n = 0;
    ssize_t len = getline(&line, &n, stdin);
    if (len < 0) {
        // EOF or a read error -- return an owned empty string rather than
        // NULL, so callers never have to special-case a null `str`.
        free(line);
        line = malloc(1);
        line[0] = '\0';
        return line;
    }
    if (len > 0 && line[len - 1] == '\n') {
        line[len - 1] = '\0';
    }
    return line;
}

// Crashes with a clear message on invalid input rather than silently
// defaulting to 0 -- there's no error-handling system in the language yet
// for a CyborgPL program to recover from this itself, and a loud failure
// right where the bad input was read is far easier to debug than a wrong
// value quietly propagating somewhere else.
double cyborg_read_num(void) {
    char *line = cyborg_read_line();
    char *end;
    double value = strtod(line, &end);
    while (*end == ' ' || *end == '\t') {
        end++;
    }
    if (end == line || *end != '\0') {
        fprintf(stderr, "input:num -- '%s' is not a valid number\n", line);
        free(line);
        exit(1);
    }
    free(line);
    return value;
}
