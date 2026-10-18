// For tests: preloaded into a test process and the audio servers it starts,
// moves their wall clock by the nanoseconds in the file named by
// WREC_TEST_WALL_CLOCK, as setting the system clock would, and leaves the
// monotonic clock alone. Every process that maps the file shares the one
// wall clock, like processes on one machine.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <sys/time.h>
#include <time.h>

static volatile int64_t *offset;

__attribute__((constructor)) static void map(void) {
    const char *path = getenv("WREC_TEST_WALL_CLOCK");
    int file = path ? open(path, O_RDONLY) : -1;
    if (file < 0)
        return;
    void *mapped = mmap(NULL, sizeof *offset, PROT_READ, MAP_SHARED, file, 0);
    if (mapped != MAP_FAILED)
        offset = mapped;
}

int clock_gettime(clockid_t clock, struct timespec *time) {
    static int (*real)(clockid_t, struct timespec *);
    if (!real)
        real = (int (*)(clockid_t, struct timespec *))dlsym(RTLD_NEXT, "clock_gettime");
    int result = real(clock, time);
    if (result == 0 && offset && (clock == CLOCK_REALTIME || clock == CLOCK_REALTIME_COARSE)) {
        int64_t nanoseconds = time->tv_sec * 1000000000LL + time->tv_nsec
            + __atomic_load_n(offset, __ATOMIC_RELAXED);
        time->tv_sec = nanoseconds / 1000000000;
        time->tv_nsec = nanoseconds % 1000000000;
    }
    return result;
}

int gettimeofday(struct timeval *time, void *zone) {
    (void)zone;
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    if (time) {
        time->tv_sec = now.tv_sec;
        time->tv_usec = now.tv_nsec / 1000;
    }
    return 0;
}

time_t time(time_t *result) {
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    if (result)
        *result = now.tv_sec;
    return now.tv_sec;
}
