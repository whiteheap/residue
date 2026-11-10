/*
 * nostatx ERRNO COMMAND [ARG...]: run COMMAND with statx(2) failing with
 * ERRNO (ENOSYS, as on Linux before 4.11, or EPERM, as under a seccomp
 * filter that refuses it), for it and every process it starts. CI's
 * "Recovery check as root, without statx" runs residue under it, so the walk
 * reads the mount table where statx gives no mount ID.
 *
 * A seccomp filter written out, so it needs no library: cc -O2 -o nostatx
 * nostatx.c. It checks that statx fails before it runs COMMAND.
 *
 * EPERM also breaks coreutils `ls`, which reports statx's EPERM as an
 * error, so the recovery check's Git wrapper, which residue's children run
 * under the same filter, cannot work under it; CI uses ENOSYS.
 */
#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#if defined(__x86_64__)
#define ARCH AUDIT_ARCH_X86_64
#elif defined(__aarch64__)
#define ARCH AUDIT_ARCH_AARCH64
#else
#error "nostatx: add this architecture's AUDIT_ARCH"
#endif

int main(int argc, char **argv) {
    if (argc < 3 || (strcmp(argv[1], "ENOSYS") != 0 && strcmp(argv[1], "EPERM") != 0)) {
        fprintf(stderr, "usage: nostatx ENOSYS|EPERM COMMAND [ARG...]\n");
        return 2;
    }
    unsigned int err = strcmp(argv[1], "EPERM") == 0 ? EPERM : ENOSYS;
    struct sock_filter filter[] = {
        /* Another architecture's calls run as they are. */
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, ARCH, 1, 0),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, __NR_statx, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (err & SECCOMP_RET_DATA)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog prog = {
        .len = (unsigned short)(sizeof filter / sizeof filter[0]),
        .filter = filter,
    };
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 ||
        prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog) != 0) {
        perror("nostatx: seccomp");
        return 3;
    }
    char buf[256];
    if (syscall(__NR_statx, AT_FDCWD, "/", 0, 0, buf) != -1 || (unsigned int)errno != err) {
        fprintf(stderr, "nostatx: statx still works under the filter\n");
        return 3;
    }
    execvp(argv[2], argv + 2);
    perror("nostatx: exec");
    return 127;
}
