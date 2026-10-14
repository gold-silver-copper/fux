"""A hidden passphrase, an arrow-key selector, a literal bracketed paste,
a prefilled line edited with Ctrl-U, then Ctrl-D on empty input."""


def solve(d, work, fixtures):
    p = d.spawn(["python3", "-I", f"{fixtures}/migration.py", work])
    d.wait_text(p, "Passphrase (hidden):", timeout=20)
    with open(f"{work}/credentials.txt", encoding="utf-8") as f:
        password = f.read().splitlines()[0]
    d.send_and_wait_text(p, ["text:" + password, "Enter"], "SELECT OPERATION:")
    d.send_and_wait_text(p, ["Right"], "[Apply]")
    d.send_and_wait_text(p, ["Enter"], "PASTE MANIFEST")
    with open(f"{work}/payload.txt", encoding="utf-8") as f:
        d.paste(p, f.read())
    d.wait_text(p, "COMMAND EDITOR>", timeout=20)
    d.send_and_wait_text(p, ["C-u", "text:apply --safe", "Enter"], "EOF TO COMMIT")
    d.keys(p, "C-d")
    status = d.wait_exit(p)
    assert status == 0, f"the migration exited {status}"
