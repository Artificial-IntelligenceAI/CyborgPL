// Backs `clock:num 'name';`. Uses CLOCK_MONOTONIC (wall-clock elapsed
// time, immune to the system clock being adjusted, unlike CLOCK_REALTIME)
// rather than clock()'s CPU time -- "how long did this take" should
// reflect real elapsed time, including time spent waiting on I/O, not just
// time actually spent executing on the CPU.
//
// The reference point is the program's actual start, captured once via a
// constructor that runs before main() -- not the first clock:num read --
// so `clock:num 'x';` gives the same answer regardless of where in the
// program it first appears.

#include <time.h>

static struct timespec start_time;

__attribute__((constructor)) static void cyborg_clock_init(void) {
    clock_gettime(CLOCK_MONOTONIC, &start_time);
}

double cyborg_clock_elapsed(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    double secs = (double)(now.tv_sec - start_time.tv_sec);
    double nsecs = (double)(now.tv_nsec - start_time.tv_nsec) / 1e9;
    return secs + nsecs;
}
