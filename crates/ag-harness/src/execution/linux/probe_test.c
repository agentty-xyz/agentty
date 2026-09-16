#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <netinet/in.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysinfo.h>
#include <sys/types.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

/* Linux keyctl ABI; musl's isolated headers do not include Linux UAPI headers. */
#define KEYCTL_GET_KEYRING_ID 0
#define KEYCTL_JOIN_SESSION_KEYRING 1
#define KEY_SPEC_SESSION_KEYRING -3

static void check(int condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "%s: errno=%d\n", message, errno);
        exit(1);
    }
}

static void readable(const char *path, int permitted) {
    int descriptor = openat(AT_FDCWD, path, O_RDONLY);
    check((descriptor >= 0) == permitted, path);
    if (descriptor >= 0) {
        char byte;
        check(read(descriptor, &byte, 1) == 1, "read positive control");
        close(descriptor);
    }
}

static void writable(const char *path, int permitted) {
    int descriptor = openat(AT_FDCWD, path, O_WRONLY);
    check((descriptor >= 0) == permitted, path);
    if (descriptor >= 0) {
        check(write(descriptor, "changed", 7) == 7, "write positive control");
        close(descriptor);
    }
}

int main(int argc, char **argv) {
    /* The trusted wrapper introduces environment state after host env_clear. */
    if (argc > 1 && strcmp(argv[1], "--unshare-user") == 0) {
        for (int index = 1; index + 1 < argc; index++) {
            if (strcmp(argv[index], "--ro-bind") == 0) {
                char gate[PATH_MAX], entered[PATH_MAX], failure[PATH_MAX], late_failure[PATH_MAX];
                check(snprintf(gate, sizeof gate, "%s/../setup-gate", argv[index + 1]) < (int)sizeof gate, "gate path");
                check(snprintf(entered, sizeof entered, "%s/../setup-entered", argv[index + 1]) < (int)sizeof entered, "entered path");
                check(snprintf(failure, sizeof failure, "%s/../setup-fail", argv[index + 1]) < (int)sizeof failure, "failure path");
                check(snprintf(late_failure, sizeof late_failure, "%s/../setup-late-fail", argv[index + 1]) < (int)sizeof late_failure, "late failure path");
                if (access(failure, F_OK) == 0) return 2;
                if (access(gate, F_OK) == 0) {
                    int marker = openat(AT_FDCWD, entered, O_CREAT | O_WRONLY, 0600);
                    check(marker >= 0, "setup entered marker");
                    close(marker);
                    while (access(gate, F_OK) == 0) usleep(10000);
                }
                check(setenv("HOST_SECRET", "must not cross", 1) == 0, "ambient environment control");
                if (access(late_failure, F_OK) == 0) {
                    char **injected = calloc((size_t)argc + 5, sizeof *injected);
                    check(injected != NULL, "failure arguments");
                    int count = 0;
                    for (int argument = 0; argument < argc; argument++) {
                        if (strcmp(argv[argument], "--") == 0) {
                            injected[count++] = "--dir";
                            injected[count++] = "/tmp/setup-reached";
                            injected[count++] = "--remount-ro";
                            injected[count++] = "/missing-mount";
                        }
                        injected[count++] = argv[argument];
                    }
                    execv(BWRAP_PATH, injected);
                    check(0, "exec late failure bubblewrap");
                }
                execv(BWRAP_PATH, argv);
                check(0, "exec bubblewrap");
            }
        }
        return 1;
    }
    if (strcmp(argv[0], "/.ag-isolation/entry") == 0) {
        check(write(1, "invalid", 7) == 7, "invalid entrypoint acknowledgement");
        return 0;
    }
    check(argc == (strcmp(argv[1], "host") == 0 ? 5 : 4), "probe arguments");
    if (strcmp(argv[1], "exit") == 0) {
        check(write(1, "payload output\n", 15) == 15, "immediate payload output");
        return 23;
    }
    if (strcmp(argv[1], "cleanup") == 0) {
        check(unlinkat(AT_FDCWD, "/.ag-isolation/entry", 0) == -1 && errno == EROFS, "entrypoint removal denied");
        check(mkdirat(AT_FDCWD, "/.ag-isolation/spoof", 0700) == -1 && errno == EROFS, "control mount write denied");
        check(mkdirat(AT_FDCWD, "/tmp/.ag-mounts-ready", 0700) == 0, "scratch marker creation control");
        check(unlinkat(AT_FDCWD, "/tmp/.ag-mounts-ready", AT_REMOVEDIR) == 0, "scratch marker removal control");
        check(symlinkat(argv[2], AT_FDCWD, "/tmp/escape") == 0, "active scratch symlink");
        readable("/tmp/escape", 0);
        writable("writable", 1);
        check(write(1, "ready\n", 6) == 6, "cleanup completed");
        int gate = 0;
        for (;;) syscall(SYS_futex, &gate, 0, 0, NULL, NULL, 0);
    }
    if (strcmp(argv[1], "pinned") == 0) {
        while (openat(AT_FDCWD, "/tmp/release", O_RDONLY) == -1) {}
        int original = openat(AT_FDCWD, "readonly", O_RDONLY);
        char byte;
        check(original >= 0 && read(original, &byte, 1) == 1 && byte == 'o', "pinned original workspace");
        close(original);
        writable("writable", 1);
        return 0;
    }
    if (strcmp(argv[1], "linger") == 0) {
        pid_t child = fork();
        check(child >= 0, "lingering descendant");
        if (child > 0) check(write(1, "ready\n", 6) == 6, "ready marker");
        int gate = 0;
        for (;;) syscall(SYS_futex, &gate, 0, 0, NULL, NULL, 0);
    }
    /* These channels remain available without syscalls and require an explicit grant. */
    check(getauxval(AT_HWCAP) != 0, "hardware auxiliary-vector positive control");
    const unsigned char *vdso = (const unsigned char *)getauxval(AT_SYSINFO_EHDR);
    check(vdso != NULL && memcmp(vdso, "\177ELF", 4) == 0, "vDSO positive control");
    int isolated = strcmp(argv[1], "isolated") == 0;
    struct utsname identity;
    struct sysinfo information;
    int result = uname(&identity);
    check(isolated ? result == -1 && errno == EPERM : result == 0, "uname boundary");
    result = sysinfo(&information);
    check(isolated ? result == -1 && errno == EPERM : result == 0, "sysinfo boundary");
    for (int domain = 0; domain < 2; domain++) {
        int descriptor = socket(domain ? AF_INET : AF_UNIX, SOCK_STREAM, 0);
        check(isolated ? descriptor == -1 && errno == EPERM : descriptor >= 0, "socket boundary");
        if (descriptor >= 0) close(descriptor);
    }
    readable(argv[2], !isolated);
    if (!isolated) {
        char inherited;
        check(read(atoi(argv[4]), &inherited, 1) == 1, "host inherited descriptor control");
        check(syscall(SYS_keyctl, KEYCTL_JOIN_SESSION_KEYRING, NULL) >= 0, "private host keyring control");
        check(syscall(SYS_add_key, "user", "probe", "x", 1, KEY_SPEC_SESSION_KEYRING) >= 0, "host add_key control");
        check(syscall(SYS_request_key, "user", "probe", NULL, KEY_SPEC_SESSION_KEYRING) >= 0, "host request_key control");
        readable("readonly", 1);
        writable("writable", 1);
        writable("readonly", 1);
        writable(".git/config", 1);
        puts("host controls passed");
        return 0;
    }
    check(getenv("HOST_SECRET") == NULL, "environment denied");
    check(getenv("ALLOWED") && strcmp(getenv("ALLOWED"), "literal value") == 0, "environment allowed");
    for (int descriptor = 0; descriptor < 64; descriptor++) {
        if (descriptor == 1 || descriptor == 2) continue;
        char byte;
        ssize_t received = read(descriptor, &byte, 1);
        if (descriptor == 0 && received == 0) continue;
        if (!(received == -1 && errno == EBADF)) fprintf(stderr, "unexpected descriptor %d, read=%zd\n", descriptor, received);
        check(received == -1 && errno == EBADF, "descriptor denied");
    }
    readable("readonly", 1);
    readable(argv[3], 1);
    writable(argv[3], 0);
    readable(".git/config", 1);
    writable("writable", 1);
    writable("readonly", 0);
    writable(".git/config", 0);
    check(renameat(AT_FDCWD, "writable", AT_FDCWD, "readonly") == -1, "rename denied");
    check(linkat(AT_FDCWD, "writable", AT_FDCWD, "alias", 0) == -1, "hardlink denied");
    check(unlinkat(AT_FDCWD, ".git/config", 0) == -1, "git unlink denied");
    check(mkdirat(AT_FDCWD, "new", 0700) == -1, "directory creation denied");
    readable("/proc/self/environ", 0);
    readable("/sys/kernel", 0);
    readable("/dev/mem", 0);
    int scratch = openat(AT_FDCWD, "/tmp/private", O_CREAT | O_RDWR, 0600);
    check(scratch >= 0 && write(scratch, "private", 7) == 7, "scratch positive control");
    close(scratch);
    check(symlinkat(argv[2], AT_FDCWD, "/tmp/escape") == 0, "scratch symlink control");
    readable("/tmp/escape", 0);
    check(syscall(SYS_keyctl, KEYCTL_GET_KEYRING_ID, KEY_SPEC_SESSION_KEYRING, 0) == -1 && errno == EPERM, "keyctl denied");
    check(syscall(SYS_add_key, "user", "probe", "x", 1, KEY_SPEC_SESSION_KEYRING) == -1 && errno == EPERM, "add_key denied");
    check(syscall(SYS_request_key, "user", "probe", NULL, KEY_SPEC_SESSION_KEYRING) == -1 && errno == EPERM, "request_key denied");
    check(syscall(SYS_unshare, 0x10000000) == -1 && errno == EPERM, "user namespace denied");
    check(syscall(SYS_clone, 0x10000000 | SIGCHLD, NULL, NULL, NULL, 0) == -1 && errno == EPERM, "clone user namespace denied");
    check(syscall(SYS_clone3, NULL, 0) == -1 && errno == EPERM, "clone3 denied");
    pid_t child = fork();
    check(child >= 0, "descendant positive control");
    if (child == 0) {
        writable("readonly", 0);
        writable("writable", 1);
        check(socket(AF_INET, SOCK_STREAM, 0) == -1 && errno == EPERM, "descendant network denied");
        _exit(0);
    }
    int status;
    check(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0, "descendant result");
    puts("isolation controls passed");
    return 0;
}
