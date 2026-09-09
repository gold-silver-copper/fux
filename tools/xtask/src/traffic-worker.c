
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/ioctl.h>
int main(void) {
    alarm(90);
    char path[4096], line[128];
    snprintf(path, sizeof path, "%s/%ld", getenv("WORKER_DIR"), (long)getpid());
    FILE *file = fopen(path, "w");
    if (!file) return 2;
    struct winsize size;
    if (ioctl(STDIN_FILENO, TIOCGWINSZ, &size)) return 3;
    fprintf(file, "%u %u\n", size.ws_col, size.ws_row);
    fclose(file);
    setbuf(stdout, NULL);
    puts("READY");
    while (fgets(line, sizeof line, stdin)) {
        for (int i = 0; i < 256; ++i) printf("%04d 012345678901234567890123456789012345678901234567890123456789\n", i);
        puts("BURST_DONE");
    }
    return 0;
}
