import os
import termios
import tty


def normalize_text(data):
    return data.replace(b'\r\n', b'\n').replace(b'\r', b'\n')


class Terminal:
    # Preserve CRLF handling across successive prompts.
    after_cr = False

    def __enter__(self):
        self.old = termios.tcgetattr(0)
        tty.setraw(0, termios.TCSANOW)
        return self

    def __exit__(self, *exc):
        termios.tcsetattr(0, termios.TCSADRAIN, self.old)

    def byte(self):
        c = os.read(0, 1)
        if not c:
            raise EOFError('Terminal input closed')
        return c

    def key(self):
        c = self.byte()
        if Terminal.after_cr:
            Terminal.after_cr = False
            if c == b'\n':
                c = self.byte()
        if c in (b'\r', b'\n'):
            Terminal.after_cr = c == b'\r'
            return b'\r'
        if c != b'\x1b':
            return c
        # Read complete escape sequences without a timing deadline. The
        # Help modal consumes its standalone Escape directly via byte().
        seq = c + self.byte()
        if seq == b'\x1bO':
            seq += self.byte()
            if seq[-1:] in b'ABCD':
                return b'\x1b[' + seq[-1:]
            return seq
        if seq != b'\x1b[':
            return seq
        seq += self.byte()
        if seq == b'\x1b[M':
            payload = self.byte() + self.byte() + self.byte()
            button, x, y = (v - 32 for v in payload)
            end = b'm' if button & 3 == 3 else b'M'
            if end == b'm':
                button = 0
            return b'\x1b[<' + f'{button};{x};{y}'.encode() + end
        if seq == b'\x1b[[':
            final = self.byte()
            return {b'A': b'\x1bOP', b'B': b'\x1bOQ'}.get(final, seq + final)
        while not 0x40 <= seq[-1] <= 0x7e:
            seq += self.byte()
        # URXVT's older numeric mouse encoding uses a button offset.
        if seq.endswith(b'M') and not seq.startswith(b'\x1b[<'):
            parts = seq[2:-1].split(b';')
            if len(parts) == 3 and all(part.isdigit() for part in parts):
                button, x, y = map(int, parts)
                button -= 32
                end = b'm' if button & 3 == 3 else b'M'
                if end == b'm':
                    button = 0
                return b'\x1b[<' + f'{button};{x};{y}'.encode() + end
        return {b'\x1b[P': b'\x1bOP', b'\x1b[Q': b'\x1bOQ',
                b'\x1b[11~': b'\x1bOP', b'\x1b[12~': b'\x1bOQ'}.get(seq, seq)

    def paste(self):
        data = bytearray()
        end = b'\x1b[201~'
        while not data.endswith(end):
            data.extend(self.byte())
        return normalize_text(bytes(data[:-len(end)]))

    def line(self, secret=False):
        buf = bytearray()
        while True:
            c = self.key()
            if c == b'\x1b[200~':
                pasted = self.paste()
                # A pasted newline submits the final line just like Enter.
                if b'\n' in pasted:
                    text = bytes(buf) + pasted.rstrip(b'\n')
                    if b'\n' in text:
                        raise ValueError('Expected one submitted line')
                    os.write(1, b'\r\n')
                    return text.decode('utf-8')
                buf.extend(pasted)
            elif c == b'\r':
                os.write(1, b'\r\n')
                return buf.decode('utf-8')
            elif c == b'\x15':
                buf.clear()
            elif c in (b'\x7f', b'\x08'):
                if buf:
                    # Remove one UTF-8 character, not an arbitrary byte.
                    text = buf.decode('utf-8', errors='replace')[:-1]
                    buf[:] = text.encode('utf-8')
            elif len(c) == 1 and c >= b' ':
                buf.extend(c)
                if not secret:
                    os.write(1, c)


def screen(body):
    os.write(1, ('\x1b[H\x1b[2J' + body.replace('\n', '\r\n')).encode())
