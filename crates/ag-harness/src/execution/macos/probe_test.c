#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <netinet/in.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int create_deep_tree(const char *path) {
    int directory = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
    if (directory < 0) return -1;
    char component[65];
    memset(component, 'd', sizeof(component) - 1);
    component[sizeof(component) - 1] = '\0';
    int result = 0;
    for (int depth = 0; depth < PATH_MAX / 64 + 4; depth++) {
        result = mkdirat(directory, component, 0700);
        if (result < 0) break;
        int next = openat(directory, component, O_RDONLY | O_DIRECTORY | O_NOFOLLOW);
        if (next < 0) {
            result = -1;
            break;
        }
        close(directory);
        directory = next;
    }
    if (result == 0) result = mkdirat(directory, "locked", 0000);
    if (result == 0) {
        int descriptor = openat(directory, "leaf", O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0600);
        if (descriptor < 0) result = -1;
        else {
            result = (int)write(descriptor, "deep", 4);
            close(descriptor);
        }
    }
    int error = errno;
    close(directory);
    errno = error;

    return result;
}

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    int result = -1;
    if (!strcmp(argv[1], "read") || !strcmp(argv[1], "write")) {
        int writing = !strcmp(argv[1], "write");
        int descriptor = open(argv[2], writing ? O_WRONLY | O_CREAT | O_TRUNC : O_RDONLY, 0600);
        if (descriptor >= 0) {
            char value;
            result = writing ? (int)write(descriptor, "changed", 7) : (int)read(descriptor, &value, 1);
            close(descriptor);
        }
    } else if (!strcmp(argv[1], "rename")) {
        result = rename(argv[2], argv[3]);
    } else if (!strcmp(argv[1], "link")) {
        result = link(argv[2], argv[3]);
    } else if (!strcmp(argv[1], "symlink")) {
        result = symlink(argv[2], argv[3]);
    } else if (!strcmp(argv[1], "fifo")) {
        result = mkfifo(argv[2], 0600);
    } else if (!strcmp(argv[1], "mkdir")) {
        result = mkdir(argv[2], 0700);
    } else if (!strcmp(argv[1], "mkdir-locked")) {
        result = mkdir(argv[2], 0000);
    } else if (!strcmp(argv[1], "deep-tree")) {
        result = create_deep_tree(argv[2]);
    } else if (!strcmp(argv[1], "chmod")) {
        result = chmod(argv[2], 0000);
    } else if (!strcmp(argv[1], "chflags")) {
        result = chflags(argv[2], UF_IMMUTABLE);
    } else if (!strcmp(argv[1], "restore-metadata")) {
        result = chflags(argv[2], 0);
        if (result == 0) result = chmod(argv[2], 0700);
    } else if (!strcmp(argv[1], "fd")) {
        result = fcntl(atoi(argv[2]), F_GETFD);
    } else if (!strcmp(argv[1], "environment")) {
        char *value = getenv(argv[2]);
        result = value && !strcmp(value, argv[3]) ? 0 : -1;
    } else if (!strcmp(argv[1], "network")) {
        int descriptor = socket(AF_INET, SOCK_STREAM, 0);
        if (descriptor >= 0) {
            struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(atoi(argv[2])), .sin_addr.s_addr = htonl(INADDR_LOOPBACK) };
            result = connect(descriptor, (struct sockaddr *)&address, sizeof(address));
            close(descriptor);
        }
    } else if (!strcmp(argv[1], "unix")) {
        int descriptor = socket(AF_UNIX, SOCK_STREAM, 0);
        if (descriptor >= 0) {
            struct sockaddr_un address = { .sun_family = AF_UNIX };
            snprintf(address.sun_path, sizeof(address.sun_path), "%s", argv[2]);
            result = connect(descriptor, (struct sockaddr *)&address, sizeof(address));
            close(descriptor);
        }
    } else if (!strcmp(argv[1], "host")) {
        char model[256];
        size_t length = sizeof(model);
        result = sysctlbyname("hw.model", model, &length, NULL, 0);
    } else if (!strcmp(argv[1], "uid")) {
        result = (int)getuid();
    } else if (!strcmp(argv[1], "spawn")) {
        pid_t child;
        char *arguments[] = { argv[0], "uid", NULL };
        int error = posix_spawn(&child, argv[0], NULL, NULL, arguments, environ);
        if (error == 0) result = waitpid(child, NULL, 0);
        else errno = error;
    } else if (!strcmp(argv[1], "fork")) {
        pid_t child = fork();
        if (child == 0) _exit(0);
        if (child > 0) result = waitpid(child, NULL, 0);
    } else if (!strcmp(argv[1], "exec-read")) {
        execl(argv[0], argv[0], "read", argv[2], NULL);
    } else if (!strcmp(argv[1], "sleep")) {
        sleep(60);
        result = 0;
    }
    if (result < 0) fprintf(stderr, "probe denied: %s\n", strerror(errno));
    return result < 0 ? 1 : 0;
}
