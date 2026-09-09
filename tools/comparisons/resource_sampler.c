/* macOS owned-process resource sampler. CPU counters use Mach absolute units;
 * --calibrate checks that conversion against CLOCK_PROCESS_CPUTIME_ID locally.
 * RSS/footprint describe one process, not its descendants or unique shared pages.
 */
#include <errno.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

static struct rusage_info_v2 sample(pid_t pid) {
    struct rusage_info_v2 info = {0};
    if (proc_pid_rusage(pid, RUSAGE_INFO_V2, (rusage_info_t *)&info)) {
        perror("proc_pid_rusage");
        exit(1);
    }
    return info;
}
static uint64_t cpu_clock(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &value)) {
        perror("clock_gettime");
        exit(1);
    }
    return (uint64_t)value.tv_sec * 1000000000 + value.tv_nsec;
}
int main(int argc, char **argv) {
    mach_timebase_info_data_t timebase;
    if (mach_timebase_info(&timebase) != KERN_SUCCESS || !timebase.denom) return 1;
    if (argc == 2 && !strcmp(argv[1], "--calibrate")) {
        uint64_t before_clock = cpu_clock();
        struct rusage_info_v2 before = sample(getpid());
        volatile uint64_t accumulator = 1;
        while (cpu_clock() - before_clock < 200000000) {
            for (int i = 0; i < 10000; ++i) accumulator = accumulator * 1664525 + 1013904223;
        }
        struct rusage_info_v2 after = sample(getpid());
        uint64_t elapsed_clock = cpu_clock() - before_clock;
        double elapsed_sample = (double)(after.ri_user_time + after.ri_system_time -
            before.ri_user_time - before.ri_system_time) * timebase.numer / timebase.denom;
        double ratio = elapsed_sample / elapsed_clock;
        printf("{\"clock_cpu_ns\":%llu,\"converted_cpu_ns\":%.0f,\"ratio\":%.6f,"
               "\"timebase_numer\":%u,\"timebase_denom\":%u}\n",
               (unsigned long long)elapsed_clock, elapsed_sample, ratio, timebase.numer, timebase.denom);
        return ratio >= .95 && ratio <= 1.05 ? 0 : 1;
    }
    if (argc != 2) return 2;
    char *end;
    errno = 0;
    long parsed = strtol(argv[1], &end, 10);
    if (errno || !argv[1][0] || *end || parsed <= 1 || parsed > INT32_MAX) return 2;
    struct rusage_info_v2 info = sample((pid_t)parsed);
    printf("{\"pid\":%ld,\"rss_bytes\":%llu,\"footprint_bytes\":%llu,"
           "\"user_ticks\":%llu,\"system_ticks\":%llu,\"start_abstime\":%llu,"
           "\"timebase_numer\":%u,\"timebase_denom\":%u}\n", parsed,
           info.ri_resident_size, info.ri_phys_footprint, info.ri_user_time,
           info.ri_system_time, info.ri_proc_start_abstime, timebase.numer, timebase.denom);
    return 0;
}
