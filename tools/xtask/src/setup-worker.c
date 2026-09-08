
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(void) {
    alarm(60);
    char line[4096];
    const char *path = getenv("WORKER_LOG");
    if (!path) return 2;
    FILE *pid = fopen(getenv("WORKER_PID"), "w");
    if (!pid) return 5;
    fprintf(pid, "%ld", (long)getpid());
    if (fclose(pid)) return 6;
    setbuf(stdout, NULL);
    puts("READY idle");
    while (fgets(line, sizeof line, stdin)) {
        FILE *out = fopen(path, "a");
        if (!out) return 3;
        fputs(line, out);
        if (fclose(out)) return 4;
        if (strncmp(line, "silent", 6)) {
            puts("RESPONSE");
            FILE *response = fopen(getenv("WORKER_RESPONSE"), "a");
            if (!response) return 7;
            fputs("response\n", response);
            if (fclose(response)) return 8;
        }
    }
    return 0;
}
