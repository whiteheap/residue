/* A process whose only use of a worktree is a file it keeps in an io_uring
 * ring, for scripts/contract-check.sh on Linux. A file in a ring has no
 * descriptor of its own: the ring's descriptor reads anon_inode:[io_uring].
 *
 *   uring-holder register FILE   opens FILE, registers it with a ring
 *                                (IORING_REGISTER_FILES) and closes it
 *   uring-holder direct FILE     opens FILE straight into a slot of the
 *                                ring's table (IORING_OP_OPENAT with a
 *                                file_index), a direct descriptor
 *   uring-holder mapped FILE     as register, then closes the ring's
 *                                descriptor too: the ring stays open, with
 *                                FILE in it, while the holder maps it
 *
 * Its working directory is /. It prints "ready" once it holds FILE, then
 * waits; where the kernel or a seccomp filter refuses io_uring (EPERM,
 * ENOSYS) it prints why and exits 77. Raw system calls, so no liburing is
 * needed. Every mode ends after 300 seconds.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef __NR_io_uring_setup
#define __NR_io_uring_setup 425
#endif
#ifndef __NR_io_uring_enter
#define __NR_io_uring_enter 426
#endif
#ifndef __NR_io_uring_register
#define __NR_io_uring_register 427
#endif
#define IORING_REGISTER_FILES 2
#define IORING_OP_OPENAT 18
#define IORING_ENTER_GETEVENTS 1u
#define IORING_FEAT_SINGLE_MMAP 1u
#define IORING_OFF_CQ_RING 0x8000000ULL
#define IORING_OFF_SQES 0x10000000ULL

/* include/uapi/linux/io_uring.h */
struct sq_off { uint32_t head, tail, ring_mask, ring_entries, flags, dropped, array, resv1; uint64_t user_addr; };
struct cq_off { uint32_t head, tail, ring_mask, ring_entries, overflow, cqes, flags, resv1; uint64_t user_addr; };
struct params { uint32_t sq_entries, cq_entries, flags, sq_thread_cpu, sq_thread_idle, features, wq_fd, resv[3]; struct sq_off sq_off; struct cq_off cq_off; };
struct sqe { uint8_t opcode, flags; uint16_t ioprio; int32_t fd; uint64_t off, addr; uint32_t len, open_flags; uint64_t user_data; uint16_t buf_index, personality; uint32_t file_index; uint64_t addr3, pad2; };
struct cqe { uint64_t user_data; int32_t res; uint32_t flags; };
_Static_assert(sizeof(struct params) == 120, "io_uring_params");
_Static_assert(sizeof(struct sqe) == 64, "io_uring_sqe");

static int die(const char *what) {
    printf("%s: %s\n", what, strerror(errno));
    return 1;
}

int main(int argc, char **argv) {
    alarm(300);
    if (argc != 3 || (strcmp(argv[1], "register") != 0 && strcmp(argv[1], "direct") != 0 && strcmp(argv[1], "mapped") != 0)) {
        fprintf(stderr, "usage: uring-holder register|direct|mapped FILE\n");
        return 2;
    }
    const char *mode = argv[1], *path = argv[2];
    int file = -1;
    if (strcmp(mode, "direct") != 0 && (file = open(path, O_RDONLY | O_CLOEXEC)) < 0) return die(path);
    if (chdir("/") != 0) return die("/");
    struct params p;
    memset(&p, 0, sizeof p);
    long rc = syscall(__NR_io_uring_setup, 4, &p);
    if (rc < 0) {
        int e = errno;
        printf("io_uring_setup: %s\n", strerror(e));
        return e == EPERM || e == ENOSYS ? 77 : 1;
    }
    int ring = (int)rc;
    size_t sq_size = p.sq_off.array + p.sq_entries * sizeof(uint32_t);
    size_t cq_size = p.cq_off.cqes + p.cq_entries * sizeof(struct cqe);
    int single = (p.features & IORING_FEAT_SINGLE_MMAP) != 0;
    if (single && cq_size > sq_size) sq_size = cq_size;
    char *sq = mmap(NULL, sq_size, PROT_READ | PROT_WRITE, MAP_SHARED, ring, 0);
    if (sq == MAP_FAILED) return die("mmap");
    char *cq = single ? sq : mmap(NULL, cq_size, PROT_READ | PROT_WRITE, MAP_SHARED, ring, IORING_OFF_CQ_RING);
    if (cq == MAP_FAILED) return die("mmap");
    /* An empty slot is -1. */
    int table[1] = {file};
    if (syscall(__NR_io_uring_register, ring, IORING_REGISTER_FILES, table, 1) < 0) return die("io_uring_register");
    if (file >= 0) close(file);
    if (strcmp(mode, "direct") == 0) {
        struct sqe *sqes = mmap(NULL, p.sq_entries * sizeof(struct sqe), PROT_READ | PROT_WRITE, MAP_SHARED, ring, IORING_OFF_SQES);
        if (sqes == MAP_FAILED) return die("mmap");
        uint32_t *tail = (uint32_t *)(sq + p.sq_off.tail), t = *tail;
        uint32_t i = t & *(uint32_t *)(sq + p.sq_off.ring_mask);
        memset(&sqes[i], 0, sizeof sqes[i]);
        sqes[i].opcode = IORING_OP_OPENAT;
        sqes[i].fd = AT_FDCWD;
        sqes[i].addr = (uint64_t)(uintptr_t)path;
        sqes[i].open_flags = O_RDONLY; /* O_CLOEXEC is refused for a slot */
        sqes[i].file_index = 1;         /* slot 0 */
        ((uint32_t *)(sq + p.sq_off.array))[i] = i;
        __atomic_store_n(tail, t + 1, __ATOMIC_RELEASE);
        if (syscall(__NR_io_uring_enter, ring, 1, 1, IORING_ENTER_GETEVENTS, NULL, 0) < 0) return die("io_uring_enter");
        uint32_t *head = (uint32_t *)(cq + p.cq_off.head), h = __atomic_load_n(head, __ATOMIC_ACQUIRE);
        if (h == __atomic_load_n((uint32_t *)(cq + p.cq_off.tail), __ATOMIC_ACQUIRE)) { printf("openat: no completion\n"); return 1; }
        struct cqe *c = (struct cqe *)(cq + p.cq_off.cqes) + (h & *(uint32_t *)(cq + p.cq_off.ring_mask));
        if (c->res < 0) { printf("openat %s: %s\n", path, strerror(-c->res)); return 1; }
        __atomic_store_n(head, h + 1, __ATOMIC_RELEASE);
    }
    if (strcmp(mode, "mapped") == 0) close(ring);
    printf("ready\n");
    fflush(stdout);
    for (;;) pause();
}
