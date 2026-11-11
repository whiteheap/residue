/* A process whose only use of a worktree is in a thread other than its
 * main one, for scripts/contract-check.sh on Linux. Linux keeps a working
 * directory and a descriptor table per thread, and /proc/PID shows only
 * the main thread's.
 *
 *   thread-holder own-cwd DIR      the main thread's working directory is
 *                                  /; a thread made with clone(2) without
 *                                  CLONE_FS has DIR as its own
 *   thread-holder own-files FILE   the main thread's working directory is
 *                                  /; a thread made without CLONE_FILES
 *                                  holds FILE open in a table of its own
 *   thread-holder main-exited DIR FILE
 *                                  works in DIR with FILE open, starts a
 *                                  thread, and ends its main thread with
 *                                  pthread_exit(3); the thread keeps both
 *   thread-holder not-dumpable DIR works in DIR, turns dumping off
 *                                  (PR_SET_DUMPABLE 0), as ssh-agent does,
 *                                  and starts a thread: the kernel shows
 *                                  every thread of it to root alone
 *
 * clone(2) rather than unshare(2), which a container's seccomp filter can
 * refuse. The cloned thread makes raw system calls only: it shares the
 * main thread's thread-local storage. Every mode ends after 300 seconds.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *target;

static int own_cwd(void *arg) {
    (void)arg;
    if (syscall(SYS_chdir, target) != 0) syscall(SYS_exit_group, 3);
    for (;;) syscall(SYS_ppoll, NULL, 0, NULL, NULL, 0);
    return 0;
}

static int own_files(void *arg) {
    (void)arg;
    if (syscall(SYS_openat, AT_FDCWD, target, O_RDONLY) < 0) syscall(SYS_exit_group, 4);
    for (;;) syscall(SYS_ppoll, NULL, 0, NULL, NULL, 0);
    return 0;
}

static void *wait_forever(void *arg) {
    (void)arg;
    for (;;) pause();
    return NULL;
}

int main(int argc, char **argv) {
    alarm(300);
    if (argc == 4 && strcmp(argv[1], "main-exited") == 0) {
        pthread_t t;
        if (chdir(argv[2]) != 0) { perror(argv[2]); return 1; }
        if (open(argv[3], O_RDONLY) < 0) { perror(argv[3]); return 1; }
        if (pthread_create(&t, NULL, wait_forever, NULL) != 0) return 1;
        pthread_exit(NULL);
    }
    if (argc == 3 && strcmp(argv[1], "not-dumpable") == 0) {
        pthread_t t;
        if (chdir(argv[2]) != 0) { perror(argv[2]); return 1; }
        if (prctl(PR_SET_DUMPABLE, 0, 0, 0, 0) != 0) { perror("prctl"); return 1; }
        if (pthread_create(&t, NULL, wait_forever, NULL) != 0) return 1;
        for (;;) pause();
    }
    if (argc != 3) {
        fprintf(stderr, "usage: thread-holder own-cwd DIR | own-files FILE | main-exited DIR FILE | not-dumpable DIR\n");
        return 2;
    }
    int shared = CLONE_VM | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM;
    int (*fn)(void *);
    if (strcmp(argv[1], "own-cwd") == 0) {
        fn = own_cwd;
        shared |= CLONE_FILES;
    } else if (strcmp(argv[1], "own-files") == 0) {
        fn = own_files;
        shared |= CLONE_FS;
    } else {
        return 2;
    }
    target = argv[2];
    if (chdir("/") != 0) { perror("/"); return 1; }
    size_t size = 1 << 16;
    char *stack = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
    if (stack == MAP_FAILED) { perror("mmap"); return 1; }
    if (clone(fn, stack + size, shared, NULL) == -1) { perror("clone"); return 1; }
    for (;;) pause();
}
