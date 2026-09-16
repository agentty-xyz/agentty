/* Trusted static entrypoint: reaching main proves bubblewrap completed setup. */
#include <errno.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 2) return 125;
    ssize_t sent;
    do {
        sent = write(STDOUT_FILENO, "\0", 1);
    } while (sent < 0 && errno == EINTR);
    if (sent != 1) return 125;

    /* execv does not reinterpret an invalid executable as a shell script. */
    execv(argv[1], argv + 1);
    return 126;
}
