/* SPDX-License-Identifier: MIT */
/* A normal, statically linked musl program: no Horn-specific userspace ABI. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/* Volatile keeps the initialized TLS loads/stores in the optimized binary. */
static _Thread_local volatile unsigned int tls_value = 42;

static void fail(const char *operation)
{
    fprintf(stderr, "musl /init: FAILED: %s (errno=%d)\n", operation, errno);
    /* PID 1 must not exit. Leave a visible failure for the boot test. */
    for (;;) pause();
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc != 1 || strcmp(argv[0], "/init") || getpid() != 1)
        fail("startup arguments/PID");
    printf("musl /init: hello from C, PID %ld\n", (long)getpid());

    if (tls_value != 42) fail("initialized TLS");
    tls_value = 123;
    errno = 0;
    if (close(-1) != -1 || errno != EBADF || tls_value != 123)
        fail("TLS/errno");
    puts("musl /init: TLS and errno work");

    size_t size = 128 * 1024;
    unsigned char *memory = malloc(size);
    if (!memory) fail("malloc");
    for (size_t i = 0; i < size; ++i) memory[i] = (unsigned char)i;
    unsigned char *larger = realloc(memory, 2 * size);
    if (!larger) fail("realloc");
    for (size_t i = 0; i < size; ++i)
        if (larger[i] != (unsigned char)i) fail("heap contents");
    free(larger);
    puts("musl /init: malloc/realloc/free work");

    FILE *file = fopen("/demo.txt", "r");
    char line[80];
    if (!file || !fgets(line, sizeof(line), file) ||
        strcmp(line, "Hello from the Horn initramfs!\n"))
        fail("initramfs read");
    if (fclose(file)) fail("fclose");
    printf("musl /init: %s", line);

    struct timespec before, after, delay = { .tv_nsec = 100000000 };
    if (clock_gettime(CLOCK_MONOTONIC, &before)) fail("clock_gettime");
    while (nanosleep(&delay, &delay))
        if (errno != EINTR) fail("nanosleep");
    if (clock_gettime(CLOCK_MONOTONIC, &after)) fail("clock_gettime");
    long long ns = (long long)(after.tv_sec - before.tv_sec) * 1000000000LL
                 + after.tv_nsec - before.tv_nsec;
    if (ns < 100000000) fail("sleep elapsed time");
    printf("musl /init: clock and sleep work (%lld us)\n", ns / 1000);
    puts("musl /init: PASS; executing task regression");
    execl("/task-test", "/task-test", (char *)NULL);
    fail("exec /task-test");
}
