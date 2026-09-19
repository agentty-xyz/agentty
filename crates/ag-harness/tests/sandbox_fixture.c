#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#ifdef __linux__
#include <signal.h>
#include <sys/syscall.h>
#endif

extern char **environ;

#ifdef LOADER_LIBRARY
__attribute__((constructor)) static void loader_effect(void) {
    FILE *outside = fopen(getenv("FORBIDDEN_PATH"), "w");
    FILE *marker = fopen(getenv("MARKER_PATH"), "w");
    /* Read-only sandboxes leave the marker unwritable; report on stdout. */
    fputs(outside ? "escaped" : "confined", marker ? marker : stdout);
    if (marker) fclose(marker);
    else fflush(stdout);
    if (outside) fclose(outside);
}
#else
static int wait_file(const char *path) {
    for (int attempt = 0; attempt < 1000 && access(path, F_OK); attempt++) {
        struct timespec delay = { .tv_sec = 0, .tv_nsec = 10000000 };
        while (nanosleep(&delay, &delay) && errno == EINTR) {}
    }
    return access(path, F_OK) ? 9 : 0;
}

static int child(const char *pid_path, const char *release, const char *result,
                 const char *forbidden) {
    char temporary[4096];
    if (snprintf(temporary, sizeof temporary, "%s.tmp", pid_path) >= sizeof temporary) return 10;
    FILE *pid = fopen(temporary, "w");
    if (!pid) return 11;
    fprintf(pid, "%d", getpid());
    fclose(pid);
    if (rename(temporary, pid_path)) return 14;
    if (wait_file(release)) return 15;
    FILE *outside = fopen(forbidden, "w");
    if (outside) { fclose(outside); return 12; }
    FILE *marker = fopen(result, "w");
    if (!marker) return 13;
    fputs("confined", marker);
    fclose(marker);
    return 0;
}

static int detached_fork(char **argv) {
    pid_t pid = fork();
    if (pid < 0) return 3;
    if (!pid) {
        if (setsid() < 0) return 4;
        close(STDOUT_FILENO);
        close(STDERR_FILENO);
        _exit(child(argv[2], argv[3], argv[4], argv[5]));
    }
    return 0;
}

static int detached_spawn(char **argv) {
    posix_spawnattr_t attributes;
    posix_spawn_file_actions_t files;
    if (posix_spawnattr_init(&attributes)) return 5;
    if (posix_spawn_file_actions_init(&files)) {
        posix_spawnattr_destroy(&attributes);
        return 5;
    }
    int status = posix_spawnattr_setflags(&attributes, POSIX_SPAWN_SETPGROUP)
        || posix_spawnattr_setpgroup(&attributes, 0)
        || posix_spawn_file_actions_addopen(&files, 1, "/dev/null", O_WRONLY, 0)
        || posix_spawn_file_actions_addopen(&files, 2, "/dev/null", O_WRONLY, 0);
    if (!status) {
        char *arguments[] = {argv[0], "child", argv[2], argv[3], argv[4], argv[5], NULL};
        pid_t pid;
        status = posix_spawn(&pid, argv[0], &files, &attributes, arguments, environ);
    }
    posix_spawn_file_actions_destroy(&files);
    posix_spawnattr_destroy(&attributes);
    return status ? 7 : 0;
}

int main(int argc, char **argv) {
#ifdef __linux__
    if (argc == 2 && !strcmp(argv[1], "keyring"))
        return syscall(SYS_keyctl, 0, -3, 0) == -1 && errno == EPERM ? 0 : 1;
    if (argc == 2 && !strcmp(argv[1], "namespace"))
        return syscall(SYS_clone, 0x10000000 | SIGCHLD, NULL, NULL, NULL, 0) == -1 && errno == EPERM ? 0 : 1;
#endif
    if (argc == 5 && !strcmp(argv[1], "--noprofile")) return 0;
    if (argc == 3 && !strcmp(argv[1], "fd"))
        return fcntl(atoi(argv[2]), F_GETFD) == -1 && errno == EBADF ? 0 : 1;
    if (argc != 6) return 2;
    if (!strcmp(argv[1], "child"))
        return child(argv[2], argv[3], argv[4], argv[5]);
    int status;
    if (!strcmp(argv[1], "fork")) status = detached_fork(argv);
    else if (!strcmp(argv[1], "spawn")) status = detached_spawn(argv);
    else return 8;
    return status ? status : wait_file(argv[2]);
}
#endif
