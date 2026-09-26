# macOS: allocating pseudo-terminals concurrently fails with errno -6, or hangs in `grantpt`

A report for Apple, ready to paste into Feedback Assistant section by section. It describes two
bugs in the macOS kernel's pseudo-terminal (PTY) driver, both of which strike when PTYs are
allocated and freed quickly, by any processes. File them as one report or two; the second section
of each is independent.

## Title

Concurrent allocation of pseudo-terminals via /dev/ptmx fails with errno -6 (kernel-private
EREDRIVEOPEN), stalls up to 400 ms, or leaves a PTY with no replica on which grantpt(3) never
returns

## Where to file it

Feedback Assistant, **macOS**, the closest area to the kernel (for example "Kernel" or "Darwin"),
type **Incorrect/Unexpected Behavior**. The filer should pick the final area.

## Summary

Programs that allocate pseudo-terminals the standard way (`posix_openpt`, `grantpt`, `unlockpt`,
`ptsname_r`, then opening the replica) hit two kernel bugs when PTYs are being allocated and freed
concurrently anywhere on the machine: in several threads of one process, or in several processes.

1. **`posix_openpt` fails with errno -6.** -6 is not a POSIX errno: it is the kernel-private
   `EREDRIVEOPEN`, which `strerror` renders as "Unknown error: -6". Many other calls take hundreds
   of milliseconds. Each failure is logged by the kernel as
   `vn_open_auth: reached max_retries error -6`.
2. **`grantpt` never returns.** Occasionally a primary opens successfully but its replica node
   (`/dev/ttysNNN`) is never created. `grantpt` on that primary then spins in the kernel, using CPU,
   until the process is killed.

**Expected:** each allocation succeeds, or fails with a POSIX errno such as `EAGAIN`, promptly;
`grantpt` on a primary returned by `posix_openpt` returns.

**Who is affected:** anything that allocates PTYs, such as terminal emulators, tmux, sshd, IDEs and
remote shells, while any other program on the machine is doing the same.

## Environment

- macOS 27.0 (26A428), `Darwin Kernel Version 27.0.0: Tue Aug 11 21:20:40 PDT 2026;
  root:xnu-13432.1.9~1/RELEASE_ARM64_T6020 arm64`
- Apple M2 Max, 12 cores
- Apple clang 16.0.0
- Control: Linux 7.0 on arm64 (Debian 12 in a VM on the same Mac) runs the first reproducer with
  no failures.

## Bug 1: `posix_openpt` fails with errno -6, or stalls

### Steps to reproduce

1. Save `ptmx_redrive.c` (below) and build it:
   `cc -O2 -Wall -Wextra -pthread ptmx_redrive.c -o ptmx_redrive`
2. Run it: `./ptmx_redrive 16 300` (16 threads, each allocating and freeing a PTY 300 times, in
   lockstep). It exits 1 if any call failed with a negative errno.
3. Read the kernel log:
   `log show --last 5m --predicate 'sender == "kernel" AND eventMessage CONTAINS "max_retries"'`

To show the race crosses processes, also run 16 copies with one thread each:
`for i in $(seq 16); do ./ptmx_redrive 1 1500 & done; wait`

### Expected results

All 4,800 allocations succeed, each in a few milliseconds.

### Actual results

```
threads 16, rounds 300, 67.1 s
allocations attempted 4800, succeeded 4756
failed: posix_openpt, errno -6 (Unknown error: -6): 44
posix_openpt calls over 50 ms: 1352, slowest 423.1 ms
```

The kernel log has one line per failure (44 here):

```
kernel: vn_open_auth: reached max_retries error -6 need_vnop_open 1 fmode 0x20003 ref_failed 0
```

Other runs:

- One process, 16 threads: 30 of 4,800 failed with -6; slowest open 441.5 ms.
- 16 processes with one thread each, 1,500 allocations apiece: 14 failed with -6 (10 of the 16
  processes affected), matched by 14 kernel log lines; slowest open 406 ms. No process has two
  threads, so the race is between processes.
- 8 processes with 4 threads each, 400 allocations apiece: 4 failed with -6.
- The same program on Linux 7.0 (arm64): 4,800 and 32,000 allocations, no failures, slowest open
  10 ms and 105 ms.

Opening `/dev/ptmx` and closing it at once, with no `grantpt`, `unlockpt` or replica, never failed
(32,000 allocations, slowest 3 ms).

### Analysis

From the newest public XNU source, `xnu-12377.1.9` (the machine runs `xnu-13432.1.9`, whose source
is not public; its behaviour matches):

- `bsd/kern/tty_ptmx.c:475`, `ptmx_clone`, the devfs clone callback for `/dev/ptmx`, returns the
  lowest free minor number without reserving it. Its own comment notes that two callers at the
  same time can be given the same minor.
- `bsd/kern/tty_ptmx.c:270`, `ptmx_get_ioctl(minor, PF_OPEN_M)`, drops `DEVFS_LOCK` to allocate
  (`kalloc_type`, `ttymalloc`), takes it again, and finds `pis_ioctl_list[minor]` taken by the
  other opener. It returns `(struct ptmx_ioctl*)-1` (lines 362–364), under the comment
  `/* Special error value so we know to redrive the open, we've been raced */`
  `/* XXX Can this still occur? */`.
- `bsd/kern/tty_dev.c:496`, `ptcopen`, turns that into `EREDRIVEOPEN` (-6,
  `bsd/sys/errno.h:280`).
- `bsd/vfs/vfs_vnops.c:770`, `vn_open_auth`, retries on `EREDRIVEOPEN`, but at most
  `max_retries = 10` times (line 388). After `RETRY_NO_YIELD_COUNT` (5, line 323) retries it
  sleeps `nretries × 10 ms` before each (lines 792–797). Past `max_retries` it logs
  `reached max_retries` and returns the error, so `EREDRIVEOPEN` reaches user space.
- Under churn every opener is handed the same lowest free minor, so the same opener can lose ten
  times in a row. Those sleeps are also why a successful open can take over 400 ms.

### Suggested fixes

- Reserve the minor in `ptmx_clone`, or allocate the `ptmx_ioctl` before choosing the minor, so
  two openers are never handed the same one.
- Never return `EREDRIVEOPEN` to user space: map an exhausted retry to `EAGAIN`.
- Retry the redrive without the growing sleeps, since the loser only needs another minor.

## Bug 2: a primary with no replica, and `grantpt` that never returns

### Steps to reproduce

1. Save `ptmx_grant_hang.c` (below) and build it:
   `cc -O2 -Wall -Wextra -pthread ptmx_grant_hang.c -o ptmx_grant_hang`
2. Run 16 copies at once, and repeat until one reports a stall (each round takes a few seconds):

   ```sh
   for round in $(seq 40); do
     for i in $(seq 16); do ./ptmx_grant_hang 1500 grant-first & done; wait
   done
   ```

   Each copy allocates and frees a PTY 1,500 times: `posix_openpt`, set close-on-exec, `grantpt`,
   `unlockpt`, name the replica with `TIOCPTYGNAME`, open it, `TIOCSWINSZ` on the primary, close
   the primary, close the replica. A watchdog thread reports any step that takes over 5 s, whether
   the replica's node exists, and the CPU the process burns. It then replaces the primary's
   descriptor with `/dev/null` (`dup2`) and reports whether the stuck call returns.

### Expected results

`grantpt` returns promptly on every primary `posix_openpt` returned, and the replica node exists.

### Actual results

```
pid 83691 STALLED in grantpt for over 5 s; replica /dev/ttys014 DOES NOT EXIST; CPU 1.06 s in the last 2 s
pid 83691 after replacing the primary with /dev/null: the call returned
```

- `grant-first`: a stall in round 2 (after 11 s). In earlier runs, rounds 7 and 10. The stuck
  process is in state R and uses about half a CPU core; a sample shows it in `__ioctl`, called from
  `grantpt`, with the program counter alternating between two instructions of the system call stub.
  The call is being restarted rather than sleeping, and replacing the descriptor ends it.
- No kernel log message accompanies the stall.
- Mode `check-first` names the replica and checks its node before `grantpt`, skipping the PTY if
  the node does not exist: 960,000 allocations, no stall, 2 PTYs skipped for a missing replica.
- The default mode (`ptsname_r` before `grantpt`, replica closed first): no stall in 480,000
  allocations. The sequence matters to how often it strikes; the missing replica is the
  precondition.

### Analysis

In `xnu-12377.1.9`:

- `bsd/kern/tty_dev.c:1091`, `TIOCPTYGRANT` (what `grantpt` calls), runs
  `_devfs_setattr(pti->pt_devhandle, …)` (line 1098).
- `bsd/kern/tty_dev.c:128`, `_devfs_setattr`, returns `ERESTART` when the replica's devfs entry is
  missing (line 148), "to redrive the grant request". `ERESTART` restarts the system call. If the
  entry never appears, the restart never ends: the thread spins until the process dies.
- The entry can be missing for good. `bsd/kern/tty_ptmx.c`, `ptmx_get_ioctl` publishes the new PTY
  in `pis_ioctl_list` (lines 370–375), drops `DEVFS_LOCK`, and only then creates the replica node
  with `devfs_make_node` (line 379), recording `NULL` if that fails (a `printf` at line 384).
  `ptmx_free_ioctl` clears the slot under the lock (line 429) but removes the old replica node only
  after dropping it (line 443). A new PTY that reuses the minor at once can therefore try to create
  `/dev/ttysNNN` while the previous PTY's node of that name is still in place. That is a likely
  way for `devfs_make_node` to fail and leave `pt_devhandle` `NULL`. In the stalls observed, the
  replica node did not exist.

### Suggested fixes

- Remove the old replica node before the minor is freed, or create the new node before the PTY is
  published, so a reused minor always gets its node.
- Fail `posix_openpt` (for example with `EAGAIN`) when `devfs_make_node` fails, rather than return
  a primary without a replica.
- Bound the `ERESTART` loop in `_devfs_setattr`: after a few restarts, fail `grantpt` with an errno
  instead of spinning.

## Attachments to add when filing

- The output of each reproducer, and the kernel log for bug 1:
  `log show --last 10m --predicate 'sender == "kernel" AND eventMessage CONTAINS "max_retries"' > kernel.log`
- For bug 2, while a copy is stuck (before its watchdog acts, within 5 s of the stall), a sample:
  `sample <pid> 3 -file stall.txt`
- Optionally a sysdiagnose (`sudo sysdiagnose`), taken right after reproducing.

## How fuxix works around it

fuxix (`fuxix::pty::open`, in https://github.com/gold-silver-copper/fux) opens one primary at a time
per process and retries -6. It names the replica and checks its node before `grantpt`, and watches
`grantpt` with a 1 s watchdog that replaces a stuck primary with `/dev/null`. A PTY without a
replica is dropped and another opened in its place.

## ptmx_redrive.c

```c
/*
 * ptmx_redrive.c: concurrent pseudo-terminal allocation on macOS.
 *
 * Every thread, in lockstep, allocates a pseudo-terminal pair the standard way
 * (posix_openpt, grantpt, unlockpt, ptsname_r, open the replica), then closes
 * both ends. On an affected kernel, some posix_openpt calls fail with errno -6,
 * which is not a POSIX errno: it is the kernel-private EREDRIVEOPEN. Others
 * take hundreds of milliseconds.
 *
 * Build: cc -O2 -Wall -Wextra -pthread ptmx_redrive.c -o ptmx_redrive
 * Run:   ./ptmx_redrive [threads (default 16)] [rounds (default 300)]
 * Exit:  1 if any call failed with a negative errno, 0 otherwise.
 */
#define _DARWIN_C_SOURCE /* ptsname_r on macOS */
#define _GNU_SOURCE      /* posix_openpt, ptsname_r on glibc (for the Linux control) */
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

/* macOS has no pthread_barrier_t, so a reusable barrier from a mutex and a condition variable. */
struct barrier {
    pthread_mutex_t lock;
    pthread_cond_t cond;
    int parties, waiting, generation;
};

static void barrier_wait(struct barrier *b) {
    pthread_mutex_lock(&b->lock);
    int generation = b->generation;
    if (++b->waiting == b->parties) {
        b->waiting = 0;
        b->generation++;
        pthread_cond_broadcast(&b->cond);
    } else {
        while (generation == b->generation)
            pthread_cond_wait(&b->cond, &b->lock);
    }
    pthread_mutex_unlock(&b->lock);
}

/* Failures, keyed by the step that failed and its errno. */
#define MAX_KINDS 32
struct failure {
    const char *step;
    int err;
    long count;
};

static struct {
    pthread_mutex_t lock;
    long attempts, succeeded, slow, negative;
    double slowest_ms;
    struct failure kinds[MAX_KINDS];
    int nkinds;
} stats = {.lock = PTHREAD_MUTEX_INITIALIZER};

static struct barrier barrier = {PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, 0, 0, 0};
static int rounds = 300;

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec * 1e3 + (double)ts.tv_nsec / 1e6;
}

static void record(const char *step, int err, double open_ms) {
    pthread_mutex_lock(&stats.lock);
    stats.attempts++;
    if (open_ms > 50.0)
        stats.slow++;
    if (open_ms > stats.slowest_ms)
        stats.slowest_ms = open_ms;
    if (step == NULL) {
        stats.succeeded++;
    } else {
        if (err < 0)
            stats.negative++;
        int i;
        for (i = 0; i < stats.nkinds; i++)
            if (stats.kinds[i].step == step && stats.kinds[i].err == err)
                break;
        if (i == stats.nkinds && stats.nkinds < MAX_KINDS)
            stats.kinds[stats.nkinds++] = (struct failure){step, err, 0};
        if (i < MAX_KINDS)
            stats.kinds[i].count++;
    }
    pthread_mutex_unlock(&stats.lock);
}

/* One allocation: the step that failed (NULL on success), its errno, and how long posix_openpt took. */
static const char *allocate(int *err, double *open_ms) {
    double start = now_ms();
    int primary = posix_openpt(O_RDWR | O_NOCTTY);
    *open_ms = now_ms() - start;
    if (primary < 0) {
        *err = errno;
        return "posix_openpt";
    }
    const char *failed = NULL;
    char name[128];
    int replica = -1;
    if (grantpt(primary) != 0)
        failed = "grantpt";
    else if (unlockpt(primary) != 0)
        failed = "unlockpt";
    else if (ptsname_r(primary, name, sizeof name) != 0)
        failed = "ptsname_r";
    else if ((replica = open(name, O_RDWR | O_NOCTTY)) < 0)
        failed = "open replica";
    if (failed != NULL)
        *err = errno;
    if (replica >= 0)
        close(replica);
    close(primary);
    return failed;
}

static void *worker(void *arg) {
    (void)arg;
    for (int round = 0; round < rounds; round++) {
        barrier_wait(&barrier);
        int err = 0;
        double open_ms = 0;
        const char *failed = allocate(&err, &open_ms);
        record(failed, err, open_ms);
    }
    return NULL;
}

int main(int argc, char **argv) {
    int threads = argc > 1 ? atoi(argv[1]) : 16;
    rounds = argc > 2 ? atoi(argv[2]) : 300;
    if (threads < 1 || rounds < 1) {
        fprintf(stderr, "usage: %s [threads] [rounds]\n", argv[0]);
        return 2;
    }
    barrier.parties = threads;
    pthread_t *ids = calloc((size_t)threads, sizeof *ids);
    if (ids == NULL)
        return 2;
    double start = now_ms();
    for (int i = 0; i < threads; i++)
        if (pthread_create(&ids[i], NULL, worker, NULL) != 0) {
            perror("pthread_create");
            return 2;
        }
    for (int i = 0; i < threads; i++)
        pthread_join(ids[i], NULL);
    double elapsed = now_ms() - start;

    printf("threads %d, rounds %d, %.1f s\n", threads, rounds, elapsed / 1e3);
    printf("allocations attempted %ld, succeeded %ld\n", stats.attempts, stats.succeeded);
    for (int i = 0; i < stats.nkinds; i++)
        printf("failed: %s, errno %d (%s): %ld\n", stats.kinds[i].step, stats.kinds[i].err,
               strerror(stats.kinds[i].err), stats.kinds[i].count);
    printf("posix_openpt calls over 50 ms: %ld, slowest %.1f ms\n", stats.slow, stats.slowest_ms);
    free(ids);
    return stats.negative > 0 ? 1 : 0;
}
```

## ptmx_grant_hang.c

```c
/*
 * ptmx_grant_hang.c: one process allocating and freeing pseudo-terminals in a loop, with a
 * watchdog. Run many copies at once. If a step takes over 5 s, the watchdog reports which, whether
 * the replica's /dev node exists, and the CPU the process burns; then it replaces the primary's
 * descriptor with /dev/null to see whether the stuck call escapes (it would, if the kernel is
 * restarting it rather than sleeping).
 *
 * Build: cc -O2 -Wall -Wextra -pthread ptmx_grant_hang.c -o ptmx_grant_hang
 * Run:   for i in $(seq 16); do ./ptmx_grant_hang 1500 grant-first & done; wait
 *        and repeat until a copy reports a stall (on the machine tested, within 10 repeats).
 * Modes: (none)        posix_openpt, ptsname_r, grantpt, unlockpt, open the replica, close the
 *                      replica, close the primary
 *        primary-first the same, closing the primary before the replica
 *        grant-first   posix_openpt, set close-on-exec, grantpt, unlockpt, TIOCPTYGNAME, open the
 *                      replica, TIOCSWINSZ on the primary, close the primary, close the replica
 *        check-first   grant-first, but after close-on-exec: TIOCPTYGNAME and stat the replica,
 *                      and skip grantpt (closing the primary) if the replica does not exist
 * Exit:  3 if a step stalled, 0 otherwise.
 */
#define _DARWIN_C_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static _Atomic(const char *) step = NULL; /* the step in progress, NULL between allocations */
static _Atomic long step_started_ms;
static _Atomic int primary_fd = -1;
static char replica_name[128];

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000L + ts.tv_nsec / 1000000L;
}

static double cpu_seconds(void) {
    struct rusage ru;
    getrusage(RUSAGE_SELF, &ru);
    return (double)(ru.ru_utime.tv_sec + ru.ru_stime.tv_sec) +
           (double)(ru.ru_utime.tv_usec + ru.ru_stime.tv_usec) / 1e6;
}

static void begin(const char *name) {
    atomic_store(&step_started_ms, now_ms());
    atomic_store(&step, name);
}

static void *watchdog(void *arg) {
    (void)arg;
    for (;;) {
        usleep(100000);
        const char *s = atomic_load(&step);
        if (s == NULL || now_ms() - atomic_load(&step_started_ms) < 5000)
            continue;
        struct stat st;
        int exists = stat(replica_name, &st) == 0;
        double cpu0 = cpu_seconds();
        sleep(2);
        double cpu1 = cpu_seconds();
        printf("pid %d STALLED in %s for over 5 s; replica %s %s; CPU %.2f s in the last 2 s\n",
               (int)getpid(), s, replica_name, exists ? "exists" : "DOES NOT EXIST", cpu1 - cpu0);
        int null = open("/dev/null", O_RDWR);
        dup2(null, atomic_load(&primary_fd));
        long waited = 0;
        while (atomic_load(&step) == s && waited < 5000) {
            usleep(10000);
            waited += 10;
        }
        printf("pid %d after replacing the primary with /dev/null: %s\n", (int)getpid(),
               atomic_load(&step) == s ? "still stuck" : "the call returned");
        fflush(stdout);
        _exit(3);
    }
    return NULL;
}

int main(int argc, char **argv) {
    int rounds = argc > 1 ? atoi(argv[1]) : 1500;
    int checked = argc > 2 && strcmp(argv[2], "check-first") == 0;
    int grant_first = checked || (argc > 2 && strcmp(argv[2], "grant-first") == 0);
    long missing = 0;
    int primary_first = grant_first || (argc > 2 && strcmp(argv[2], "primary-first") == 0);
    pthread_t dog;
    pthread_create(&dog, NULL, watchdog, NULL);
    long failures = 0;
    for (int i = 0; i < rounds; i++) {
        begin("posix_openpt");
        int primary = posix_openpt(O_RDWR | O_NOCTTY);
        if (primary < 0) {
            failures++;
            atomic_store(&step, NULL);
            continue;
        }
        atomic_store(&primary_fd, primary);
        if (grant_first) {
            begin("fcntl FD_CLOEXEC");
            fcntl(primary, F_SETFD, fcntl(primary, F_GETFD) | FD_CLOEXEC);
            if (checked) {
                begin("TIOCPTYGNAME");
                struct stat st;
                if (ioctl(primary, TIOCPTYGNAME, replica_name) != 0 || stat(replica_name, &st) != 0) {
                    missing++;
                    atomic_store(&step, NULL);
                    close(primary);
                    continue;
                }
            }
            begin("grantpt");
            int ok = grantpt(primary) == 0;
            begin("unlockpt");
            ok = ok && unlockpt(primary) == 0;
            begin("TIOCPTYGNAME");
            ok = ok && ioctl(primary, TIOCPTYGNAME, replica_name) == 0;
            int replica = -1;
            if (ok) {
                begin("open replica");
                replica = open(replica_name, O_RDWR | O_NOCTTY | O_CLOEXEC);
            }
            if (replica >= 0) {
                begin("TIOCSWINSZ");
                struct winsize ws = {24, 80, 0, 0};
                ioctl(primary, TIOCSWINSZ, &ws);
            }
            atomic_store(&step, NULL);
            close(primary);
            if (replica >= 0)
                close(replica);
            continue;
        }
        begin("ptsname_r");
        if (ptsname_r(primary, replica_name, sizeof replica_name) != 0)
            replica_name[0] = 0;
        begin("grantpt");
        int ok = grantpt(primary) == 0;
        begin("unlockpt");
        ok = ok && unlockpt(primary) == 0;
        int replica = -1;
        if (ok) {
            begin("open replica");
            replica = open(replica_name, O_RDWR | O_NOCTTY);
        }
        atomic_store(&step, NULL);
        if (primary_first) {
            close(primary);
            if (replica >= 0)
                close(replica);
        } else {
            if (replica >= 0)
                close(replica);
            close(primary);
        }
    }
    printf("pid %d done: %d rounds, %ld posix_openpt failures, %ld replicas missing\n", (int)getpid(),
           rounds, failures, missing);
    return 0;
}
```
