"""Drives a full-screen terminal program through a pseudo-terminal and reads its screen as text.

Only the Python standard library is used, so it runs in the dev VM after `dev-vm.sh mirror python3`.
A small terminal emulator applies the cursor moves and erases a ratatui program sends, so tests
search the screen a person would see, not the raw byte stream. Every byte the program wrote is
also kept, so a test can prove a secret never reached the terminal in any form.

    with Tui(["nodo-tui"]) as tui:
        tui.wait_for("Device passphrase")
        tui.type("secret", enter=True)

A failed wait raises `ScreenTimeout`, whose message holds the last screen.
"""

import codecs
import fcntl
import os
import pty
import re
import select
import signal
import struct
import termios
import time
import unicodedata

KEYS = {
    "enter": "\r",
    "esc": "\x1b",
    "tab": "\t",
    "backtab": "\x1b[Z",
    "backspace": "\x7f",
    "up": "\x1b[A",
    "down": "\x1b[B",
    "right": "\x1b[C",
    "left": "\x1b[D",
    "space": " ",
    "ctrl-c": "\x03",
    "ctrl-u": "\x15",
    "ctrl-s": "\x13",
}


class ScreenTimeout(AssertionError):
    pass


def char_width(ch):
    if unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Screen:
    """Just enough of a VT100 for crossterm: cursor moves, erases, scroll and SGR (ignored)."""

    def __init__(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.clear()
        self.saved = (0, 0)
        self.state = "text"
        self.params = ""

    def clear(self):
        self.grid = [[" "] * self.cols for _ in range(self.rows)]
        self.row = self.col = 0

    def scroll_up(self):
        self.grid.pop(0)
        self.grid.append([" "] * self.cols)

    def put(self, ch):
        width = char_width(ch)
        if width == 0:
            return
        if self.col + width > self.cols:
            self.col = 0
            self.newline()
        self.grid[self.row][self.col] = ch
        if width == 2 and self.col + 1 < self.cols:
            self.grid[self.row][self.col + 1] = ""
        self.col += width

    def newline(self):
        if self.row == self.rows - 1:
            self.scroll_up()
        else:
            self.row += 1

    def feed(self, text):
        for ch in text:
            if self.state == "text":
                if ch == "\x1b":
                    self.state = "esc"
                elif ch == "\r":
                    self.col = 0
                elif ch == "\n":
                    self.newline()
                elif ch == "\b":
                    self.col = max(0, self.col - 1)
                elif ch == "\t":
                    self.col = min(self.cols - 1, (self.col // 8 + 1) * 8)
                elif ch >= " " and ch != "\x7f":
                    self.put(ch)
            elif self.state == "esc":
                if ch == "[":
                    self.state, self.params = "csi", ""
                elif ch == "]":
                    self.state = "osc"
                elif ch in "()":
                    self.state = "charset"
                elif ch == "7":
                    self.saved, self.state = (self.row, self.col), "text"
                elif ch == "8":
                    (self.row, self.col), self.state = self.saved, "text"
                else:
                    self.state = "text"
            elif self.state == "charset":
                self.state = "text"
            elif self.state == "osc":
                if ch == "\x07":
                    self.state = "text"
                elif ch == "\x1b":
                    self.state = "osc-esc"
            elif self.state == "osc-esc":
                self.state = "text" if ch == "\\" else "osc"
            elif self.state == "csi":
                if "\x40" <= ch <= "\x7e":
                    self.csi(self.params, ch)
                    self.state = "text"
                else:
                    self.params += ch

    def csi(self, params, final):
        private = params.startswith("?")
        nums = [int(p) if p.isdigit() else 0 for p in params.lstrip("?").split(";")] if params else []

        def arg(i, default=1):
            return nums[i] if len(nums) > i and nums[i] else default

        if final in "Hf":
            self.row = min(self.rows - 1, arg(0) - 1)
            self.col = min(self.cols - 1, arg(1) - 1)
        elif final == "A":
            self.row = max(0, self.row - arg(0))
        elif final == "B":
            self.row = min(self.rows - 1, self.row + arg(0))
        elif final == "C":
            self.col = min(self.cols - 1, self.col + arg(0))
        elif final == "D":
            self.col = max(0, self.col - arg(0))
        elif final == "G":
            self.col = min(self.cols - 1, arg(0) - 1)
        elif final == "d":
            self.row = min(self.rows - 1, arg(0) - 1)
        elif final == "J":
            mode = arg(0, 0)
            if mode == 2 or mode == 3:
                self.grid = [[" "] * self.cols for _ in range(self.rows)]
            elif mode == 0:
                self.grid[self.row][self.col:] = [" "] * (self.cols - self.col)
                for r in range(self.row + 1, self.rows):
                    self.grid[r] = [" "] * self.cols
            elif mode == 1:
                for r in range(self.row):
                    self.grid[r] = [" "] * self.cols
                self.grid[self.row][: self.col + 1] = [" "] * (self.col + 1)
        elif final == "K":
            mode = arg(0, 0)
            line = self.grid[self.row]
            if mode == 0:
                line[self.col:] = [" "] * (self.cols - self.col)
            elif mode == 1:
                line[: self.col + 1] = [" "] * (self.col + 1)
            else:
                self.grid[self.row] = [" "] * self.cols
        elif final == "s":
            self.saved = (self.row, self.col)
        elif final == "u":
            self.row, self.col = self.saved
        elif final in "hl" and private and 1049 in nums:
            self.clear()

    def text(self):
        return "\n".join("".join(line).rstrip() for line in self.grid)


class Tui:
    def __init__(self, argv, rows=32, cols=100, env=None, log=None):
        self.rows, self.cols = rows, cols
        self.screen = Screen(rows, cols)
        self.raw = bytearray()
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.log = log
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            environment = {"TERM": "xterm-256color", "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "LANG": "C.UTF-8"}
            environment.update(env or {})
            try:
                os.execvpe(argv[0], argv, environment)
            finally:
                os._exit(127)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.status = None

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def pump(self, seconds):
        """Reads whatever the program writes for up to `seconds`. Returns False once it has exited."""
        end = time.monotonic() + seconds
        while True:
            left = end - time.monotonic()
            if left <= 0:
                return True
            ready, _, _ = select.select([self.fd], [], [], left)
            if not ready:
                return True
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                data = b""
            if not data:
                self.reap()
                return False
            self.raw += data
            if self.log:
                self.log.write(data)
            self.screen.feed(self.decoder.decode(data))

    def reap(self):
        if self.status is None:
            try:
                pid, status = os.waitpid(self.pid, 0)
                self.status = os.waitstatus_to_exitcode(status)
            except ChildProcessError:
                self.status = -1
        return self.status

    def text(self):
        return self.screen.text()

    def send(self, *keys, pause=0.15):
        """Sends named keys ("enter", "down", ...) or literal text, one at a time."""
        for key in keys:
            os.write(self.fd, KEYS.get(key, key).encode())
            self.pump(pause)

    def type(self, text, enter=False):
        """Types text a character at a time, as a person would, so no paste handling is involved."""
        for ch in text:
            os.write(self.fd, ch.encode())
            self.pump(0.02)
        if enter:
            self.send("enter")

    def contains(self, needle):
        screen = self.text()
        if isinstance(needle, re.Pattern):
            return needle.search(screen) is not None
        return needle in screen

    def wait_for(self, *needles, timeout=15, absent=()):
        """Waits until every needle is on screen and none of `absent` is. Returns the screen."""
        end = time.monotonic() + timeout
        while True:
            if all(self.contains(n) for n in needles) and not any(self.contains(n) for n in absent):
                return self.text()
            if time.monotonic() > end:
                missing = [str(n) for n in needles if not self.contains(n)]
                present = [str(n) for n in absent if self.contains(n)]
                raise ScreenTimeout(
                    f"waited {timeout}s; missing {missing}; unexpectedly present {present}\n"
                    f"--- last screen ---\n{self.text()}\n-------------------"
                )
            if not self.pump(0.1):
                if not all(self.contains(n) for n in needles):
                    raise ScreenTimeout(f"the program exited ({self.status})\n--- last screen ---\n{self.text()}")

    def wait_gone(self, *needles, timeout=15):
        return self.wait_for(timeout=timeout, absent=needles)

    def wait_exit(self, timeout=10):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if not self.pump(0.1):
                return self.status
        raise ScreenTimeout(f"the program did not exit\n--- last screen ---\n{self.text()}")

    def close(self):
        if self.status is None:
            try:
                os.kill(self.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.wait_exit(5)
            except ScreenTimeout:
                os.kill(self.pid, signal.SIGKILL)
                self.reap()
        try:
            os.close(self.fd)
        except OSError:
            pass
