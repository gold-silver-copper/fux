/* Types keys into an X display through XTest: each line of stdin is
 * MODS KEYSYM, MODS a set of C, M, S (or -), KEYSYM an X keysym name.
 * Each key is pressed with its modifiers held, then released, and the
 * program waits GAP milliseconds before the next. */
#include <X11/Xlib.h>
#include <X11/keysym.h>
#include <X11/extensions/XTest.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>

static void key(Display *d, KeyCode k, Bool down) {
    XTestFakeKeyEvent(d, k, down, CurrentTime);
}

int main(int argc, char **argv) {
    int gap = argc > 1 ? atoi(argv[1]) : 100;
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "no display\n"); return 1; }
    KeyCode shift = XKeysymToKeycode(d, XK_Shift_L);
    KeyCode ctrl = XKeysymToKeycode(d, XK_Control_L);
    KeyCode alt = XKeysymToKeycode(d, XK_Alt_L);
    /* The pointer onto the terminal's window, which has the focus. */
    XTestFakeMotionEvent(d, -1, 40, 40, CurrentTime);
    XFlush(d);
    usleep(200 * 1000);
    char mods[16], name[64];
    while (scanf("%15s %63s", mods, name) == 2) {
        KeySym sym = XStringToKeysym(name);
        KeyCode k = sym == NoSymbol ? 0 : XKeysymToKeycode(d, sym);
        if (!k) { fprintf(stderr, "no key %s\n", name); continue; }
        /* A keysym on a key's shifted level needs Shift to be typed. */
        Bool c = strchr(mods, 'C') != NULL, m = strchr(mods, 'M') != NULL;
        Bool s = strchr(mods, 'S') != NULL;
        struct timespec now;
        clock_gettime(CLOCK_REALTIME, &now);
        printf("%.3f %s %s\n", now.tv_sec + now.tv_nsec / 1e9, mods, name);
        fflush(stdout);
        if (c) key(d, ctrl, True);
        if (m) key(d, alt, True);
        if (s) key(d, shift, True);
        key(d, k, True);
        key(d, k, False);
        if (s) key(d, shift, False);
        if (m) key(d, alt, False);
        if (c) key(d, ctrl, False);
        XFlush(d);
        usleep(gap * 1000);
    }
    XCloseDisplay(d);
    return 0;
}
