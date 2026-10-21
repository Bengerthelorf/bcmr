#!/usr/bin/env python3
"""Tree selection and portable metadata contracts; only SSH is replaced.

Run with BCMR=/absolute/path/to/bcmr python3 tests/remote_tree_parity_test.py.
The fixture executes real shells and local serve subprocesses, never a server
from the user's SSH configuration.
"""
import os
import shlex
import stat
import unittest

import destination_semantics_test as destinations
import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem and shell")
class RemoteTreeParityTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    assert_files = fixture.SourceStarTests.assert_files
    route = destinations.DestinationSemanticsTests.route

    MODES = destinations.DestinationSemanticsTests.MODES
    CASES = tuple((mode, parallel) for mode in MODES for parallel in (1, 2)
                  if mode != "local" or parallel == 1)

    def route_case(self, mode, parallel):
        # Keep the transport suffix while giving each case independent paths.
        operation, separator, transport = mode.partition("-")
        name = operation + "-p" + str(parallel)
        if separator:
            name += "-" + transport
        return self.route(name)

    def make_directories_writable(self):
        for path in [self.root, *self.root.rglob("*")]:
            if path.is_dir() and not path.is_symlink():
                path.chmod(0o700)

    # Missing directory finalization loses root, nested and empty metadata;
    # early chmod or utime prevents writes or changes the final directory mtime.
    def test_preserve_directory_modes_and_mtimes_after_content(self):
        self.addCleanup(self.make_directories_writable)
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                tree = src / "tree"
                self.put(tree, "child/payload.txt", "complete tree bytes")
                (tree / "empty").mkdir()
                expected = (("", 0o550, 1_600_000_001),
                            ("child", 0o550, 1_600_000_002),
                            ("empty", 0o500, 1_600_000_003))
                for relative, mode_bits, mtime in expected:
                    path = tree / relative
                    path.chmod(mode_bits)
                    os.utime(path, (mtime, mtime))
                self.assert_ok(self.run_bcmr("copy", "-r", "-p", "-P", parallel,
                                             source(tree), target(out)))
                landed = out / "tree"
                self.assertEqual((landed / "child/payload.txt").read_text(), "complete tree bytes")
                for relative, mode_bits, mtime in expected:
                    observed = (landed / relative).stat()
                    self.assertEqual(stat.S_IMODE(observed.st_mode), mode_bits, str(landed / relative))
                    self.assertEqual(int(observed.st_mtime), mtime, str(landed / relative))

    def test_preserve_an_empty_root_directory(self):
        self.addCleanup(self.make_directories_writable)
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                tree = src / "empty"
                tree.mkdir()
                tree.chmod(0o510)
                os.utime(tree, (1_600_000_004, 1_600_000_004))
                self.assert_ok(self.run_bcmr("copy", "-r", "-p", "-P", parallel,
                                             source(tree), target(out)))
                landed = out / "empty"
                observed = landed.stat()
                self.assertEqual(stat.S_IMODE(observed.st_mode), 0o510)
                self.assertEqual(int(observed.st_mtime), 1_600_000_004)
                self.assertEqual(list(landed.iterdir()), [])

    # Flat per-row filtering must not recreate an excluded parent for its leaf.
    def test_excluded_ancestor_prunes_files_and_empty_descendants(self):
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                tree = src / "tree"
                self.put(tree, "keep.txt", "selected")
                self.put(tree, "omit/child.txt", "excluded")
                (tree / "omit/deep-empty").mkdir()
                (tree / "keep-empty").mkdir()
                self.assert_ok(self.run_bcmr("copy", "-r", "-P", parallel,
                                             "--exclude", r"(^|/)omit$", source(tree), target(out)))
                self.assert_files(out, {"tree/keep.txt": "selected"})
                self.assertTrue((out / "tree/keep-empty").is_dir())
                self.assertFalse((out / "tree/omit").exists())

    # Pruning must happen before a followed link inside that subtree is read.
    def test_excluded_ancestor_hides_a_cyclic_link(self):
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                tree = src / "tree"
                self.put(tree, "keep.txt", "selected")
                (tree / "omit").mkdir()
                (tree / "omit/loop").symlink_to("..", target_is_directory=True)
                self.assert_ok(self.run_bcmr("copy", "-r", "-P", parallel,
                                             "--exclude", r"(^|/)omit$", source(tree), target(out)))
                self.assert_files(out, {"tree/keep.txt": "selected"})
                self.assertFalse((out / "tree/omit").exists())

    # Every transport must apply the same root exclusion before transferring.
    def test_excluded_top_level_file_is_not_copied(self):
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                skip = self.put(src, "skip.txt", "excluded root")
                keep = self.put(src, "keep.txt", "selected root")
                self.assert_ok(self.run_bcmr("copy", "-P", parallel, "--exclude", r"skip\.txt$",
                                             source(skip), source(keep), target(out)))
                self.assert_files(out, {"keep.txt": "selected root"})

    def test_excluded_root_directory_does_not_create_target(self):
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                tree = src / "omit"
                self.put(tree, "payload.txt", "excluded tree")
                destination = out / "missing"
                self.assert_ok(self.run_bcmr("copy", "-r", "-P", parallel,
                                             "--exclude", r"(^|/)omit$", source(tree), target(destination, slash=True)))
                self.assertFalse(destination.exists())

    def test_dry_run_uses_the_same_pruned_inventory(self):
        for mode, parallel in self.CASES:
            with self.subTest(mode=mode, parallel=parallel):
                src, out, source, target = self.route_case(mode, parallel)
                tree = src / "tree"
                self.put(tree, "keep.txt", "selected")
                self.put(tree, "omit/child.txt", "excluded")
                result = self.run_bcmr("copy", "-r", "--dry-run", "-P", parallel,
                                       "--exclude", r"(^|/)omit$", source(tree), target(out))
                self.assert_ok(result)
                output = result.stdout + result.stderr
                self.assertIn("keep.txt", output)
                self.assertNotIn("child.txt", output)
                if mode != "local":
                    self.assertRegex(output, r"would (?:up|down)load 8 B to ")
                self.assertEqual(list(out.iterdir()), [])

    # The sequential legacy path used to ignore a child mkdir exit code.
    def test_legacy_empty_child_mkdir_failure_is_reported_sequentially(self):
        tree = self.local / "tree"
        (tree / "child").mkdir(parents=True)
        destination = self.home / "parent"
        blocker = self.put(destination, "tree/child", "original blocker")
        result = self.run_bcmr("copy", "-r", "-P", "1", tree, f"legacy:{destination}")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(blocker.read_text(), "original blocker")

    # A successful chmod must never conceal a failed timestamp operation.
    def test_upload_reports_timestamp_failure(self):
        marker = self.root / "touch-failed"
        touch = self.bin / "touch"
        touch.write_text("#!/bin/sh\nprintf reached > " + shlex.quote(str(marker))
                         + "\nprintf 'fixture timestamp failure\\n' >&2\nexit 9\n")
        touch.chmod(0o755)
        for mode in ("upload-serve", "upload-legacy"):
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "payload.txt", "attribute bytes")
                result = self.run_bcmr("copy", "-p", source(file), target(out))
                self.assertTrue(marker.exists(), "timestamp failure injection was not reached")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_failed_upload_metadata_keeps_existing_target(self):
        touch = self.bin / "touch"
        touch.write_text("#!/bin/sh\nprintf 'fixture timestamp failure\\n' >&2\nexit 9\n")
        touch.chmod(0o755)
        for mode in ("upload-serve", "upload-legacy"):
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "payload.txt", "new attribute bytes")
                destination = self.put(out, "payload.txt", "old destination bytes")
                result = self.run_bcmr("copy", "-p", "-f", "-y", source(file), target(out))
                self.assertEqual(destination.read_text(), "old destination bytes")
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
