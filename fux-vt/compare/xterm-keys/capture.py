# Runs inside xterm: writes the modes to set (argv[2], escaped), puts the
# tty in raw mode, and records every read with its time to argv[1], until
# nothing has come for 4 s after the first byte.
import os, sys, time, tty, termios, select
out = open(sys.argv[1], 'w')
setup = sys.argv[2].encode().decode('unicode_escape').encode('latin-1')
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(sys.stdout.fileno(), setup)
out.write('ready\n'); out.flush()
start = None
try:
    while True:
        r, _, _ = select.select([fd], [], [], 4.0 if start else 30.0)
        if not r:
            break
        data = os.read(fd, 1024)
        start = start or time.time()
        out.write('%.3f %s\n' % (time.time(), data.hex()))
        out.flush()
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
    out.close()
