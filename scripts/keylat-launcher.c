// The app that macOS is asked to let watch the screen and press keys, for
// scripts/keylat.swift: it runs the program named by its arguments and waits
// for it. Permission is given to a program as it was built, so one given to
// the measuring tool itself would be lost every time the tool is changed;
// this never changes, and what it runs acts under its permission, as a
// program run from a terminal acts under the terminal's.
//
// Run with no arguments it asks for the two permissions, which is what puts
// it in the lists under Privacy & Security, and exits with 0 if it has both
// and 3 if not.

#include <ApplicationServices/ApplicationServices.h>
#include <spawn.h>
#include <stdbool.h>
#include <sys/wait.h>

extern char **environ;

int main(int argc, char **argv) {
    if (argc < 2) {
        bool watch = CGPreflightScreenCaptureAccess();
        bool type = CGPreflightPostEventAccess();
        if (!watch) CGRequestScreenCaptureAccess();
        if (!type) CGRequestPostEventAccess();
        return watch && type ? 0 : 3;
    }

    pid_t pid;
    if (posix_spawn(&pid, argv[1], NULL, NULL, argv + 1, environ) != 0) return 2;
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) return 2;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 2;
}
