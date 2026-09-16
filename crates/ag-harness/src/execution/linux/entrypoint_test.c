/* Exercise the real entrypoint with controlled syscall results. */
#define main entrypoint_main
#define write fixture_write
#define execv fixture_execv
#include "entrypoint.c"
#undef execv
#undef write
#undef main

#include <assert.h>
#include <string.h>

static int interrupts;
static int fail_write;
static int executions;
static char **expected_arguments;

ssize_t fixture_write(int descriptor, const void *buffer, size_t length) {
    assert(descriptor == STDOUT_FILENO && length == 1);
    assert(*(const char *)buffer == '\0');
    if (interrupts > 0) {
        interrupts--;
        errno = EINTR;
        return -1;
    }
    if (fail_write) {
        errno = EBADF;
        return -1;
    }
    return 1;
}

int fixture_execv(const char *path, char *const arguments[]) {
    assert(strcmp(path, "payload") == 0);
    assert(arguments == expected_arguments);
    executions++;
    errno = ENOEXEC;
    return -1;
}

int main(void) {
    /* Arrange */
    char *arguments[] = {"entrypoint", "payload", "argument", NULL};
    expected_arguments = arguments + 1;

    /* Act / Assert */
    assert(entrypoint_main(1, arguments) == 125 && executions == 0);
    fail_write = 1;
    assert(entrypoint_main(3, arguments) == 125 && executions == 0);
    fail_write = 0;
    interrupts = 1;
    assert(entrypoint_main(3, arguments) == 126 && executions == 1);
    assert(interrupts == 0);
    return 0;
}
