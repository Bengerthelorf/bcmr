#!/usr/bin/env python3
"""Real PTY regressions for shell-friendly progress and honest summaries."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import tempfile
import termios
import time
import unittest

BCMR = str(Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve())


def screen_text(data, width):
    # The inline renderer uses only relative cursor moves and per-line clearing.
    # Model those controls, including deferred terminal line wrapping.
    rows = [[]]
    row = col = 0
    text = data.decode("utf-8", errors="replace")
    tokens = re.split(r"(\x1b\[[0-?]*[ -/]*[@-~])", text)
    for token in tokens:
        if token.startswith("\x1b["):
            match = re.fullmatch(r"\x1b\[(\d*)([AGK])", token)
            if not match:
                continue
            count = int(match[1] or "1")
            if match[2] == "A":
                row = max(0, row - count)
            elif match[2] == "G":
                col = max(0, count - 1)
            elif match[2] == "K":
                rows[row] = [] if count == 2 else rows[row][:col]
            continue
        for char in token:
            if char == "\r":
                col = 0
            elif char == "\n":
                row += 1
            elif char >= " ":
                if col >= width:
                    row += 1
                    col = 0
                while len(rows) <= row:
                    rows.append([])
                while len(rows[row]) <= col:
                    rows[row].append(" ")
                rows[row][col] = char
                col += 1
            while len(rows) <= row:
                rows.append([])
    return ["".join(line).rstrip() for line in rows]


@unittest.skipUnless(os.name == "posix", "requires a POSIX pseudo-terminal")
class TerminalUxTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="bcmr-terminal-ux-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = dict(os.environ, HOME=str(self.root),
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        TERM="xterm-ghostty", TERM_PROGRAM="ghostty")
        for key in ("NO_COLOR", "BCMR_CONFIG", "BCMR_PROFILE"):
            self.env.pop(key, None)

    def terminal(self, *args, width=100, height=40, interrupt_after=None):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
        os.write(slave, b"SHELL_HISTORY_SENTINEL\n$ bcmr command\n")
        process = subprocess.Popen([BCMR, *map(str, args)], cwd=self.root, env=self.env,
                                   stdin=slave, stdout=slave, stderr=slave)
        os.close(slave)
        data = bytearray()
        try:
            started = time.monotonic()
            interrupted = False
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if interrupt_after is not None and not interrupted and time.monotonic() - started >= interrupt_after:
                    os.write(master, b"\x03")
                    interrupted = True
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    data.extend(chunk)
                elif process.poll() is not None:
                    break
            self.assertIsNotNone(process.poll(), "CLI did not return from PTY operation")
            return process.wait(), bytes(data)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)

    def test_auto_progress_stays_below_command_without_switching_screens(self):
        source = self.root / "payload"
        source.write_bytes(b"a" * 128 * 1024)
        rc, output = self.terminal("copy", "--bandwidth-limit=128KiB/s", source, "copied")
        self.assertEqual(rc, 0, output)
        for control in (b"\x1b[?1049h", b"\x1b[2J", b"\x1b[1;1H"):
            self.assertFalse(control in output, "auto progress must preserve the main terminal screen")
        screen = screen_text(output, 100)
        self.assertEqual(screen[:2], ["SHELL_HISTORY_SENTINEL", "$ bcmr command"])
        self.assertIn("Done:", "\n".join(screen[2:]))

    def test_explicit_inline_refresh_does_not_overwrite_shell_history(self):
        source = self.root / "payload"
        source.write_bytes(b"a" * 128 * 1024)
        rc, output = self.terminal("copy", "--progress=inline", "--bandwidth-limit=128KiB/s",
                                   source, "copied")
        self.assertEqual(rc, 0, output)
        self.assertEqual(screen_text(output, 100)[:2],
                         ["SHELL_HISTORY_SENTINEL", "$ bcmr command"])

    @unittest.skipIf(hasattr(os, "geteuid") and os.geteuid() == 0,
                     "requires a non-root user for a mode-bit permission failure")
    def test_plain_error_has_no_success_summary(self):
        source = self.root / "source"
        source.write_text("payload")
        protected = self.root / "protected"
        protected.mkdir()
        protected.chmod(0o500)
        self.addCleanup(protected.chmod, 0o700)
        result = subprocess.run([BCMR, "copy", "--progress=plain", str(source), str(protected / "out")],
                                cwd=self.root, env=self.env, capture_output=True,
                                text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Done:", result.stdout)

    def test_explicit_tui_interrupt_cleans_stages_and_never_claims_done(self):
        source = self.root / "payload"
        source.write_bytes(b"a" * 512 * 1024)
        rc, output = self.terminal("copy", "--progress=tui", "--bandwidth-limit=64KiB/s",
                                   source, "copied", interrupt_after=0.4)
        self.assertEqual(rc, 130, output[-1000:])
        self.assertNotIn(b"Done:", output)
        self.assertFalse((self.root / "copied").exists())
        self.assertEqual(list(self.root.glob(".bcmr*")), [])

    def test_explicit_tui_remains_available(self):
        source = self.root / "payload"
        source.write_text("payload")
        rc, output = self.terminal("copy", "--progress=tui", source, "copied")
        self.assertEqual(rc, 0, output[-1000:])
        self.assertIn(b"\x1b[?1049h", output)
        self.assertIn(b"\x1b[?1049l", output)

    def test_narrow_terminal_uses_plain_without_cursor_controls(self):
        source = self.root / "payload"
        source.write_text("payload")
        rc, output = self.terminal("copy", source, "copied", width=25, height=3)
        self.assertEqual(rc, 0, output)
        self.assertNotIn(b"\x1b", output)

    def test_verbose_output_stays_in_scrollback_without_live_cursor_moves(self):
        tree = self.root / "tree"
        tree.mkdir()
        for name in ("first.txt", "second.txt"):
            (tree / name).write_text("payload")
        for operation in ("copy", "move", "remove"):
            with self.subTest(operation=operation):
                selected = self.root / operation
                subprocess.run(["cp", "-R", str(tree), str(selected)], check=True)
                args = [operation, "-rv"]
                if operation == "remove":
                    args += ["-y", selected]
                elif operation == "move":
                    destination = self.root / "move-out"
                    destination.mkdir()
                    args += [selected / "first.txt", selected / "second.txt", destination]
                else:
                    args += [selected, str(self.root / (operation + "-out"))]
                rc, output = self.terminal(*args)
                self.assertEqual(rc, 0, output[-1000:])
                self.assertNotIn(b"\x1b[3A", output)
                for name in (b"first.txt", b"second.txt"):
                    self.assertIn(name, output)

    def test_remove_plain_summary_reports_items_even_for_empty_directories(self):
        directory = self.root / "empty"
        directory.mkdir()
        result = subprocess.run([BCMR, "remove", "-ry", "--progress=plain", str(directory)],
                                cwd=self.root, env=self.env, capture_output=True,
                                text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stdout, r"1 (?:item|director)")

    def test_quiet_wins_over_verbose_for_all_three_operations(self):
        for operation in ("copy", "move", "remove"):
            with self.subTest(operation=operation):
                source = self.root / (operation + "-source")
                source.write_text("payload")
                args = [operation, "-qv", str(source)]
                if operation == "remove":
                    args.append("-y")
                else:
                    args.append(str(self.root / (operation + "-target")))
                result = subprocess.run([BCMR, *args], cwd=self.root, env=self.env,
                                        capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(result.stdout + result.stderr, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
