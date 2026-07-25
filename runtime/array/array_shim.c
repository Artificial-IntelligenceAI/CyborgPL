// Backs `var:array:TYPE`. A type-erased, growable, homogeneous buffer --
// codegen already knows the concrete element type at every call site
// (opaque pointers mean it can `load`/`store` through a raw slot pointer
// directly, no casting needed), so this shim only ever needs to know the
// element size in bytes, not what kind of element it actually is. Same
// role a minimal `Vec<T>` plays in Rust, just type-erased the way C
// dynamic-array libraries usually are.
//
// Freeing *elements* (for str/bignum/file element types, which each own
// their own separate heap allocation) is entirely codegen's job -- it
// iterates 1..=length and frees each slot's content itself before calling
// cyborg_array_free, which only ever frees this buffer's own memory.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    void *data;
    size_t length;    // elements currently in use
    size_t capacity;  // elements the buffer can hold before growing
    size_t elem_size; // bytes per element
} CyborgArray;

void *cyborg_array_new(size_t elem_size) {
    CyborgArray *arr = malloc(sizeof(CyborgArray));
    arr->data = NULL;
    arr->length = 0;
    arr->capacity = 0;
    arr->elem_size = elem_size;
    return arr;
}

void cyborg_array_free(void *handle) {
    CyborgArray *arr = (CyborgArray *)handle;
    free(arr->data);
    free(arr);
}

static void cyborg_array_ensure_capacity(CyborgArray *arr, size_t needed) {
    if (needed <= arr->capacity) {
        return;
    }
    size_t new_cap = arr->capacity == 0 ? 4 : arr->capacity * 2;
    if (new_cap < needed) {
        new_cap = needed;
    }
    arr->data = realloc(arr->data, new_cap * arr->elem_size);
    arr->capacity = new_cap;
}

// Copies `elem_size` bytes from `value_ptr` onto the end, growing the
// buffer first if needed.
void cyborg_array_append(void *handle, void *value_ptr) {
    CyborgArray *arr = (CyborgArray *)handle;
    cyborg_array_ensure_capacity(arr, arr->length + 1);
    memcpy((char *)arr->data + arr->length * arr->elem_size, value_ptr, arr->elem_size);
    arr->length++;
}

// Returns a pointer to element `index` (1-based -- the first element is
// index 1, not 0), crashing with a clear message on an out-of-range index
// rather than reading/writing memory that isn't actually part of the
// array.
void *cyborg_array_get_ptr(void *handle, long index) {
    CyborgArray *arr = (CyborgArray *)handle;
    if (index < 1 || (size_t)index > arr->length) {
        fprintf(stderr, "array index %ld out of range (length %zu)\n", index, arr->length);
        exit(1);
    }
    return (char *)arr->data + (size_t)(index - 1) * arr->elem_size;
}

long cyborg_array_length(void *handle) {
    return (long)((CyborgArray *)handle)->length;
}
