#!/usr/bin/env python3
"""Removal UX regressions against real files, CLI processes and terminal input.

BCMR=/absolute/path/to/bcmr python3 tests/remove_ux_test.py
"""
import json
import fcntl
import os
import pty
import select
import signal
import struct
import subprocess
import termios
import time
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX links, permissions and terminal input")
class RemoveUxTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    def run_interactive(self, answers, *args):
        master, slave = pty.openpty()
        child = None
        try:
            child = subprocess.Popen([fixture.BCMR, "remove", "--progress=off", *map(str, args)],
                                     env=self.env, cwd=self.local, stdin=slave,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            os.close(slave)
            slave = None
            os.write(master, answers.encode())
            stdout, stderr = child.communicate(timeout=10)
            return subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.communicate()
            if slave is not None:
                os.close(slave)
            os.close(master)

    # Terminal separators must never turn unlinking a symlink into traversing it.
    def test_directory_symlink_spelling_removes_only_link(self):
        for index, suffix in enumerate(("", "/", "///")):
            with self.subTest(suffix=suffix):
                target = self.local / f"target-{index}"
                payload = self.put(target, "nested/payload", "referent survives")
                link = self.local / f"link-{index}"
                link.symlink_to(target, target_is_directory=True)
                self.assert_ok(self.run_bcmr("remove", "-ry", str(link) + suffix))
                self.assertTrue(payload.exists(), "removing the link deleted its referent")
                self.assertEqual(payload.read_text(), "referent survives")
                self.assertFalse(link.is_symlink())

    def test_dangling_symlink_can_be_removed_with_terminal_separators(self):
        for index, suffix in enumerate(("", "/", "///")):
            with self.subTest(suffix=suffix):
                link = self.local / f"dangling-{index}"
                link.symlink_to("missing target")
                self.assert_ok(self.run_bcmr("remove", "-y", str(link) + suffix))
                self.assertFalse(link.is_symlink())

    # A contents-first walk must prune exclusions before visiting descendants.
    def test_excluding_directory_preserves_all_its_descendants(self):
        tree = self.local / "tree"
        survivor = self.put(tree, "keep/nested/private", "keep these bytes")
        doomed = self.put(tree, "gone", "remove these bytes")
        self.assert_ok(self.run_bcmr("remove", "-ry", "-e", r"(^|/)keep$", tree))
        self.assertTrue(survivor.exists(), "excluded directory contents were removed")
        self.assertEqual(survivor.read_text(), "keep these bytes")
        self.assertFalse(doomed.exists())
        self.assertTrue(tree.is_dir())

    def test_dry_run_excludes_directory_contents_and_never_prompts(self):
        tree = self.local / "tree"
        survivor = self.put(tree, "keep/private", "keep")
        doomed = self.put(tree, "gone", "preview only")
        result = self.run_bcmr("remove", "-rin", "-e", r"(^|/)keep$", tree)
        self.assert_ok(result)
        self.assertNotIn(str(survivor), result.stdout)
        self.assertIn(str(doomed), result.stdout)
        self.assertNotIn("(y/N)", result.stdout)
        self.assertEqual(survivor.read_text(), "keep")
        self.assertEqual(doomed.read_text(), "preview only")

    @unittest.skipIf(not hasattr(os, "geteuid") or os.geteuid() == 0,
                     "requires non-root permission enforcement")
    def test_force_reports_permission_error_instead_of_silently_skipping(self):
        parent = self.local / "private"
        payload = self.put(parent, "payload", "still here")
        parent.chmod(0)
        self.addCleanup(parent.chmod, 0o700)
        for flag in ("-f", "-y"):
            with self.subTest(flag=flag):
                result = self.run_bcmr("remove", flag, payload)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertRegex(result.stderr.lower(), r"permission denied|not permitted")
        parent.chmod(0o700)
        self.assertEqual(payload.read_text(), "still here")

    def test_force_missing_is_success_and_missing_without_force_preserves_other_inputs(self):
        missing = self.local / "missing"
        self.assert_ok(self.run_bcmr("remove", "-f", missing))
        keep = self.put(self.local, "keep", "keep on preflight failure")
        result = self.run_bcmr("remove", "-y", keep, missing)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(keep.read_text(), "keep on preflight failure")

    def test_yes_neutralizes_interactive_without_a_terminal(self):
        for index, flags in enumerate(("-iy", "-if")):
            with self.subTest(flags=flags):
                payload = self.put(self.local, f"payload-{index}", "remove")
                self.assert_ok(self.run_bcmr("remove", flags, payload))
                self.assertFalse(payload.exists())

    def test_json_verbose_stdout_remains_only_json_records(self):
        for recursive in (False, True):
            with self.subTest(recursive=recursive):
                payload = self.put(self.local, f"json-{recursive}/payload", "remove")
                target = payload.parent if recursive else payload
                flags = ["-r"] if recursive else []
                result = self.run_bcmr("remove", "--json", "-vy", *flags, target)
                self.assert_ok(result)
                records = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertTrue(any(record.get("type") == "result" and
                                    record.get("status") == "success" for record in records))
                self.assertFalse(target.exists())

    def test_quiet_suppresses_verbose_real_removal_output(self):
        payload = self.put(self.local, "quiet", "remove")
        result = self.run_bcmr("remove", "-qvy", payload)
        self.assert_ok(result)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "")
        self.assertFalse(payload.exists())

    def test_final_json_progress_counts_root_and_empty_directories(self):
        for name, expected_items in (("empty", 1), ("recursive", 4)):
            with self.subTest(name=name):
                directory = self.local / name
                directory.mkdir()
                if name == "recursive":
                    self.put(directory, "nested/payload", "remove")
                    (directory / "empty-child").mkdir()
                result = self.run_bcmr("remove", "-ry", "--json", directory)
                self.assert_ok(result)
                records = [json.loads(line) for line in result.stdout.splitlines()]
                progress = [record for record in records if record.get("type") == "progress"]
                self.assertTrue(progress, "successful removal needs a final progress snapshot")
                self.assertEqual(progress[-1]["items_total"], expected_items)
                self.assertEqual(progress[-1]["items_done"], expected_items)
                self.assertFalse(directory.exists())

    def test_interactive_multiple_files_accept_or_skip_each_once(self):
        keep = self.put(self.local, "keep", "keep")
        remove = self.put(self.local, "remove", "remove")
        result = self.run_interactive("n\ny\n", "-iv", keep, remove)
        self.assert_ok(result)
        self.assertEqual(keep.read_text(), "keep")
        self.assertFalse(remove.exists())
        self.assertEqual(result.stdout.count("(y/N)"), 2)
        self.assertNotIn("Do you want to proceed", result.stdout)

    def test_interactive_recursive_tree_prompts_once_per_entry(self):
        tree = self.local / "tree"
        self.put(tree, "payload", "remove")
        result = self.run_interactive("y\ny\n", "-ri", tree)
        self.assert_ok(result)
        self.assertFalse(tree.exists())
        self.assertEqual(result.stdout.count("(y/N)"), 2)

    def test_interactive_tui_option_leaves_terminal_modes_and_cursor_unchanged(self):
        payload = self.put(self.local, "payload", "remove after confirmation")
        master, slave = pty.openpty()
        child = None
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            before = termios.tcgetattr(slave)
            env = dict(self.env, TERM="xterm")
            env.pop("NO_COLOR", None)
            child = subprocess.Popen([fixture.BCMR, "remove", "-i", "--progress=tui", str(payload)],
                                     env=env, cwd=self.local, stdin=slave,
                                     stdout=slave, stderr=slave)
            output = bytearray()
            answered = False
            deadline = time.monotonic() + 5
            while child.poll() is None and time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    output.extend(os.read(master, 65536))
                    if not answered and b"(y/N)" in output:
                        os.write(master, b"y\n")
                        answered = True
            self.assertIsNotNone(child.poll(), "interactive removal did not complete")
            while select.select([master], [], [], 0)[0]:
                output.extend(os.read(master, 65536))
            self.assertEqual(child.returncode, 0, output)
            self.assertTrue(answered, output)
            self.assertFalse(payload.exists())
            self.assertEqual(termios.tcgetattr(slave), before,
                             "confirmation left the caller's terminal in raw mode")
            self.assertNotIn(b"\x1b[?25l", output,
                             "suppressed TUI must not leave the cursor hidden")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.wait()
            os.close(slave)
            os.close(master)

    def test_directory_requires_recursive_and_dir_only_requires_empty(self):
        tree = self.local / "tree"
        payload = self.put(tree, "payload", "keep")
        for flags in ("-y", "-dy"):
            with self.subTest(flags=flags):
                result = self.run_bcmr("remove", flags, tree)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(payload.read_text(), "keep")
        empty = self.local / "empty"
        empty.mkdir()
        self.assert_ok(self.run_bcmr("remove", "-dy", empty))
        self.assertFalse(empty.exists())

    def test_dir_only_does_not_traverse_children_created_during_confirmation(self):
        directory = self.local / "empty-at-plan"
        directory.mkdir()
        master, slave = pty.openpty()
        child = None
        try:
            child = subprocess.Popen([fixture.BCMR, "remove", "-d", "--progress=off", str(directory)],
                                     env=self.env, cwd=self.local, stdin=slave,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            os.close(slave)
            slave = None
            output = bytearray()
            deadline = time.monotonic() + 5
            while b"Do you want to proceed?" not in output and time.monotonic() < deadline:
                if select.select([child.stdout], [], [], 0.1)[0]:
                    data = os.read(child.stdout.fileno(), 65536)
                    if not data:
                        break
                    output.extend(data)
            self.assertIn(b"Do you want to proceed?", output)
            payload = self.put(directory, "arrived-after-plan", "must survive -d")
            os.write(master, b"y\n")
            stdout, stderr = child.communicate(timeout=5)
            self.assertNotEqual(child.returncode, 0, bytes(output) + stdout + stderr)
            self.assertIn(b"not empty", stderr.lower())
            self.assertTrue(directory.is_dir())
            self.assertEqual(payload.read_text(), "must survive -d")
        finally:
            if child is not None and child.poll() is None:
                child.kill()
                child.communicate()
            if slave is not None:
                os.close(slave)
            os.close(master)

    # Every operand is checked before the first deletion. These are confined
    # temporary trees; filesystem-root refusal is tested only by Rust validation.
    def test_dot_and_dotdot_operands_are_refused_before_any_deletion(self):
        for index, spelling in enumerate(("tree/.", "tree/.///", "tree/../")):
            with self.subTest(spelling=spelling):
                base = self.local / f"case-{index}"
                first = self.put(base, "first", "first operand survives")
                payload = self.put(base, "tree/payload", "tree payload survives")
                result = self.run_bcmr("remove", "-ry", first, str(base) + "/" + spelling)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertRegex(result.stderr, r"refus|ending in|\.\.")
                self.assertTrue(first.exists(), "validation happened after earlier deletion")
                self.assertTrue(payload.exists(), "dot operand deleted directory contents")

    def test_named_entry_after_parent_component_is_still_removable(self):
        directory = self.local / "sub"
        directory.mkdir()
        payload = self.put(self.local, "named", "remove named entry")
        self.assert_ok(self.run_bcmr("remove", "-y", str(directory) + "/../named"))
        self.assertFalse(payload.exists())

    def test_sigint_stops_before_later_removals(self):
        first = self.put(self.local, "first", "first")
        later = self.put(self.local, "later", "later")
        child = subprocess.Popen([fixture.BCMR, "remove", "--progress=plain", "--json",
                                  "-y", "--test-mode=delay:3000", str(first), str(later)],
                                 env=self.env, cwd=self.local, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            # A real progress frame synchronizes the signal with the delay hook.
            deadline = time.monotonic() + 5
            output = b""
            while b'"bytes_done":5' not in output and time.monotonic() < deadline:
                if select.select([child.stdout], [], [], 0.1)[0]:
                    chunk = os.read(child.stdout.fileno(), 65536)
                    if not chunk:
                        break
                    output += chunk
            self.assertIn(b'"bytes_done":5', output)
            child.send_signal(signal.SIGINT)
            stdout, stderr = child.communicate(timeout=5)
            self.assertNotEqual(child.returncode, 0, output + stdout + stderr)
            self.assertEqual(later.read_text(), "later")
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate()


if __name__ == "__main__":
    unittest.main(verbosity=2)
