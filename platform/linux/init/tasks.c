/* SPDX-License-Identifier: MIT */
#define _GNU_SOURCE
#include <errno.h>
#include <fenv.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static _Thread_local volatile unsigned tls_value = 42;
static _Thread_local volatile sig_atomic_t signals_seen;
static volatile unsigned cow_value = 7;

static void fail(const char *operation)
{
    fprintf(stderr, "linux tasks: FAILED: %s (errno=%d)\n", operation, errno);
    if (getpid() != 1) _exit(111);
    for (;;) pause();
}
#define CHECK(test) do { if (!(test)) fail(#test); } while (0)
#define PTHREAD(call) do { int error = (call); if (error) { errno = error; fail(#call); } } while (0)

static void sleep_ns(long ns)
{
    struct timespec delay = { .tv_nsec = ns };
    while (nanosleep(&delay, &delay)) CHECK(errno == EINTR);
}
static void wait_status(pid_t child, int expected)
{
    int status;
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == expected);
}
static void fork_test(void)
{
    int go[2];
    CHECK(pipe(go) == 0);
    unsigned char *memory = malloc(8192);
    CHECK(memory != NULL);
    memset(memory, 0x5a, 8192);
    tls_value = 101;
    pid_t child = fork();
    CHECK(child >= 0);
    if (!child) {
        char token;
        close(go[1]);
        CHECK(read(go[0], &token, 1) == 1);
        CHECK(cow_value == 7 && tls_value == 101);
        for (size_t i = 0; i < 8192; ++i) CHECK(memory[i] == 0x5a);
        cow_value = 99;
        memory[0] = 99;
        tls_value = 99;
        _exit(23);
    }
    close(go[0]);
    cow_value = 17;
    memset(memory, 0xa5, 8192);
    CHECK(write(go[1], "!", 1) == 1);
    close(go[1]);
    wait_status(child, 23);
    CHECK(cow_value == 17 && tls_value == 101);
    for (size_t i = 0; i < 8192; ++i) CHECK(memory[i] == 0xa5);
    free(memory);
    child = fork();
    CHECK(child >= 0);
    if (!child) {
        execl("/task-test", "/task-test", "--exec-child", (char *)NULL);
        fail("exec child");
    }
    wait_status(child, 27);
    puts("linux tasks: fork/exec/wait and bidirectional COW work");

    /* Retiring translation objects must not leak the allocating RAM account. */
    for (int i = 0; i < 64; ++i) {
        child = fork();
        CHECK(child >= 0);
        if (!child) _exit(i);
        wait_status(child, i);
    }
    puts("linux tasks: 64 fork/reap cycles work");

    void *guard = mmap(NULL, 4096, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    CHECK(guard != MAP_FAILED);
    child = fork();
    CHECK(child >= 0);
    if (!child) { unsigned value = *(volatile unsigned *)guard; _exit(value); }
    int status;
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV);
    CHECK(munmap(guard, 4096) == 0);
    puts("linux tasks: child protection fault is contained");
}

#define THREADS 3
static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t condition = PTHREAD_COND_INITIALIZER;
static unsigned ready, go, counter;
static long tids[THREADS];
static const int rounding[THREADS] = { FE_DOWNWARD, FE_UPWARD, FE_TOWARDZERO };
static void signal_handler(int signal)
{
    if (signal == SIGUSR1) ++signals_seen;
}
static void *worker(void *argument)
{
    unsigned id = (uintptr_t)argument;
    CHECK(tls_value == 42 && signals_seen == 0);
    tls_value = 1000 + id;
    tids[id] = syscall(SYS_gettid);
    CHECK(fesetround(rounding[id]) == 0);
    PTHREAD(pthread_mutex_lock(&mutex));
    ++ready;
    PTHREAD(pthread_cond_broadcast(&condition));
    while (!go) PTHREAD(pthread_cond_wait(&condition, &mutex));
    PTHREAD(pthread_mutex_unlock(&mutex));
    for (unsigned i = 0; i < 32; ++i) {
        errno = EDOM + id;
        CHECK(sched_yield() == 0);
        sleep_ns(100000);
        CHECK(errno == (int)(EDOM + id) && tls_value == 1000 + id);
        CHECK(fegetround() == rounding[id]);
        PTHREAD(pthread_mutex_lock(&mutex));
        /* Deliberately block other contenders: exercise futex wait/wake. */
        if (!i) sleep_ns(2000000);
        ++counter;
        PTHREAD(pthread_mutex_unlock(&mutex));
        unsigned char *memory = malloc(1024 + id);
        CHECK(memory != NULL);
        memset(memory, id, 1024 + id);
        free(memory);
    }
    PTHREAD(pthread_kill(pthread_self(), SIGUSR1));
    CHECK(signals_seen == 1 && tls_value == 1000 + id);
    CHECK(fegetround() == rounding[id]);
    return (void *)(uintptr_t)(id + 1);
}
static void pthread_test(void)
{
    struct sigaction action = { .sa_handler = signal_handler };
    CHECK(sigemptyset(&action.sa_mask) == 0);
    CHECK(sigaction(SIGUSR1, &action, NULL) == 0);
    for (unsigned round = 0; round < 4; ++round) {
        pthread_t threads[THREADS];
        ready = go = counter = 0;
        PTHREAD(pthread_mutex_lock(&mutex));
        for (unsigned i = 0; i < THREADS; ++i)
            PTHREAD(pthread_create(&threads[i], NULL, worker, (void *)(uintptr_t)i));
        while (ready != THREADS) PTHREAD(pthread_cond_wait(&condition, &mutex));
        go = 1;
        PTHREAD(pthread_cond_broadcast(&condition));
        PTHREAD(pthread_mutex_unlock(&mutex));
        for (unsigned i = 0; i < THREADS; ++i) {
            void *result;
            PTHREAD(pthread_join(threads[i], &result));
            CHECK(result == (void *)(uintptr_t)(i + 1));
            CHECK(tids[i] > 1);
            for (unsigned j = 0; j < i; ++j) CHECK(tids[i] != tids[j]);
        }
        CHECK(counter == THREADS * 32 && tls_value == 101 && signals_seen == 0);
        CHECK(fegetround() == FE_TONEAREST);
    }
    PTHREAD(pthread_cond_destroy(&condition));
    PTHREAD(pthread_mutex_destroy(&mutex));
    puts("linux tasks: pthread TLS/errno, mutex/condvar, join, signals and FP isolation work");
}
int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc == 2 && !strcmp(argv[1], "--exec-child")) {
        CHECK(getpid() != 1 && tls_value == 42);
        puts("linux tasks: exec child has fresh TLS");
        return 27;
    }
    CHECK(getpid() == 1);
    puts("linux tasks: starting single-CPU task tests");
    fork_test();
    pthread_test();
    puts("linux tasks: PASS; executing FP/SIMD regression");
    execl("/fp-probe", "/fp-probe", (char *)NULL);
    fail("exec /fp-probe");
}
