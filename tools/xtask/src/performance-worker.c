
#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(void) {
    alarm(90); setbuf(stdout, NULL); char line[64]; puts("READY");
    while (fgets(line, sizeof line, stdin)) {
        line[strcspn(line, "\r\n")] = 0;
        int slow = line[0] == 'S';
        for (int i = 0; i < (slow ? 100 : 1000); i++) {
            printf("%04d abcdefghijklmnopqrstuvwxyz\n", i);
            if (slow) usleep(10000);
        }
        printf("DONE_%s\n", line);
    }
    return 0;
}
