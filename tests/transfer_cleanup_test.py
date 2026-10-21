#!/usr/bin/env python3
"""Move cleanup and overwrite consent through real CLI/SSH boundary fixtures."""
import os
from pathlib import Path
import pty
import subprocess
import unittest

import star_copy_test as fixture
import remote_overwrite_test as overwrite_fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX shell and PTY")
class TransferCleanupTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    inject_transport_before_exec = overwrite_fixture.RemoteOverwriteTests.inject_transport_before_exec

    def route(self, mode):
        source = self.put(self.home if mode.startswith("download") else self.local,
                          mode + "/source.txt", "new bytes")
        destination = self.put(self.home if mode.startswith("upload") else self.dest,
                               mode + "/target.txt", "old bytes")
        host = "legacy" if mode.endswith("legacy") else "fixture"
        return (source, destination,
                f"{host}:{source}" if mode.startswith("download") else str(source),
                f"{host}:{destination}" if mode.startswith("upload") else str(destination))

    def test_remote_force_requires_the_same_nonterminal_consent_as_local(self):
        for mode in ("upload-serve", "upload-legacy", "download-serve", "download-legacy"):
            with self.subTest(mode=mode):
                source, destination, src, dst = self.route(mode)
                result = self.run_bcmr("copy", "-f", src, dst)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("confirmation", result.stdout + result.stderr)
                self.assertEqual(destination.read_text(), "old bytes")
                self.assertEqual(source.read_text(), "new bytes")

    def test_dry_run_does_not_request_overwrite_consent(self):
        for mode in ("local", "upload-serve", "upload-legacy", "download-serve", "download-legacy"):
            with self.subTest(mode=mode):
                source, destination, src, dst = self.route(mode)
                self.assert_ok(self.run_bcmr("copy", "-f", "--dry-run", src, dst))
                self.assertEqual(destination.read_text(), "old bytes")
                self.assertEqual(source.read_text(), "new bytes")

    def test_declining_remote_move_preserves_both_source_and_destination(self):
        for mode in ("upload-serve", "upload-legacy", "download-serve", "download-legacy"):
            with self.subTest(mode=mode):
                source, destination, src, dst = self.route(mode)
                master, slave = pty.openpty()
                try:
                    child = subprocess.Popen(
                        [fixture.BCMR, "move", "--progress=off", "-f", src, dst],
                        cwd=self.local, env=self.env, stdin=slave,
                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    os.close(slave)
                    slave = None
                    os.write(master, b"n\n")
                    stdout, stderr = child.communicate(timeout=30)
                    self.assertNotEqual(child.returncode, 0, stdout + stderr)
                    self.assertTrue(source.exists(), "declined move removed its source")
                    self.assertEqual(source.read_text(), "new bytes")
                    self.assertEqual(destination.read_text(), "old bytes")
                finally:
                    if slave is not None:
                        os.close(slave)
                    os.close(master)

    def test_remote_move_with_excluded_ancestor_only_removes_selected_entries(self):
        for mode in ("upload-serve", "upload-legacy", "download-serve", "download-legacy"):
            with self.subTest(mode=mode):
                base = (self.home if mode.startswith("download") else self.local) / mode
                self.put(base, "tree/move.txt", "move")
                self.put(base, "tree/keep/private.txt", "private")
                output = (self.home if mode.startswith("upload") else self.dest) / mode
                host = "legacy" if mode.endswith("legacy") else "fixture"
                src = f"{host}:{base / 'tree'}" if mode.startswith("download") else base / "tree"
                dst = f"{host}:{output}/" if mode.startswith("upload") else str(output) + "/"
                result = self.run_bcmr("move", "-r", "--exclude", r"(^|/)keep$", src, dst)
                self.assert_ok(result)
                self.assertEqual((output / "tree/move.txt").read_text(), "move")
                self.assertFalse((output / "tree/keep").exists())
                self.assertFalse((base / "tree/move.txt").exists())
                self.assertEqual((base / "tree/keep/private.txt").read_text(), "private")

    def test_fanout_move_with_excludes_preserves_unselected_source_tree(self):
        tree = self.local / "tree"
        self.put(tree, "move.txt", "move")
        self.put(tree, "keep/private.txt", "private")
        outputs = [self.dest / "one", self.dest / "two"]
        args = ["-r", "--exclude", r"(^|/)keep$", str(tree)]
        for output in outputs:
            args += ["--to", str(output) + "/"]
        self.assert_ok(self.run_bcmr("move", *args))
        for output in outputs:
            self.assertEqual((output / "tree/move.txt").read_text(), "move")
            self.assertFalse((output / "tree/keep").exists())
        self.assertEqual((tree / "keep/private.txt").read_text(), "private")
        self.assertFalse((tree / "move.txt").exists())

    def test_fanout_cleanup_does_not_discover_and_delete_late_files(self):
        tree = self.local / "tree"
        self.put(tree, "move.txt", "move")
        late = tree / "arrived-later.txt"
        env = dict(self.env, STAR_LATE_SOURCE=str(late))
        self.assert_ok(self.run_bcmr(
            "move", "-r", tree, "--to", f"fixture:{self.home / 'out'}/",
            "--to", str(self.dest / "out") + "/", env=env))
        self.assertTrue(late.exists(), "cleanup recursively deleted a newly arrived file")
        self.assertEqual(late.read_text(), "created after source expansion")
        self.assertFalse((tree / "move.txt").exists())

    def test_changed_source_is_preserved_after_remote_copy(self):
        source = self.put(self.local, "source.txt", "original bytes")
        destination = self.home / "target.txt"
        marker = self.root / "changed-source-fault-reached"
        self.inject_transport_before_exec('''
if (words == ['bcmr', 'serve'] or
        (len(words) == 1 and words[0].startswith('cat > '))) and not os.path.exists(%r):
    with open(%r, 'w') as stream:
        stream.write('payload transfer reached')
    with open(%r, 'w') as stream:
        stream.write('modified bytes')
''' % (str(marker), str(marker), str(source)))
        result = self.run_bcmr("move", source, f"fixture:{destination}")
        self.assertTrue(marker.exists(), "source mutation fault was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(source.exists(), "move deleted an uncaptured source version")
        self.assertEqual(source.read_text(), "modified bytes")

    def test_same_length_source_rewrite_with_restored_mtime_is_preserved(self):
        for mode in ("serve", "legacy"):
            with self.subTest(mode=mode):
                source = self.put(self.local, mode + "-source.txt", "original bytes")
                destination = self.home / (mode + "-target.txt")
                marker = self.root / (mode + "-same-length-fault-reached")
                original = source.stat()
                self.inject_transport_before_exec('''
if (words == ['bcmr', 'serve'] or
        (len(words) == 1 and words[0].startswith('cat > '))) and not os.path.exists(%r):
    with open(%r, 'w') as stream:
        stream.write('same-length source mutation reached')
    saved = os.stat(%r)
    with open(%r, 'w') as stream:
        stream.write('modified bytes')
    os.utime(%r, ns=(saved.st_atime_ns, saved.st_mtime_ns))
''' % (str(marker), str(marker), str(source), str(source), str(source)))
                host = "fixture" if mode == "serve" else "legacy"
                result = self.run_bcmr("move", source, f"{host}:{destination}")
                self.assertTrue(marker.exists(), "source mutation fault was not reached")
                self.assertTrue(source.exists(), "cleanup deleted a changed source with restored mtime")
                self.assertEqual(source.read_text(), "modified bytes")
                self.assertEqual(source.stat().st_mtime_ns, original.st_mtime_ns)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_fanout_move_cleans_every_selected_hardlink(self):
        source = self.local / "hardlinks"
        first = self.put(source, "first", "hardlinked source bytes")
        (source / "nested").mkdir()
        os.link(first, source / "nested/second")
        outputs = [self.dest / "one", self.dest / "two"]
        args = ["-r", source]
        for output in outputs:
            args += ["--to", str(output) + "/"]
        self.assert_ok(self.run_bcmr("move", *args))
        self.assertFalse(source.exists())
        for output in outputs:
            for relative in ("first", "nested/second"):
                self.assertEqual((output / "hardlinks" / relative).read_text(),
                                 "hardlinked source bytes")


if __name__ == "__main__":
    unittest.main(verbosity=2)
