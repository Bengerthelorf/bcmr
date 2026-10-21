#!/usr/bin/env python3
"""Compare destination behavior across local, serve, and legacy transfers.

Set BCMR to the binary under test. The shared star fixture replaces only SSH;
all filesystem operations, remote shell commands, and bcmr serve are real.
"""
import os
from pathlib import Path
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem and shell")
class DestinationSemanticsTests(unittest.TestCase):
    # Reuse only fixture construction and process/filesystem helpers. Inheriting
    # its TestCase would accidentally run all star regressions in this suite.
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    assert_files = fixture.SourceStarTests.assert_files

    MODES = ("local", "upload-serve", "upload-legacy", "download-serve", "download-legacy")

    def route(self, mode):
        upload = mode.startswith("upload")
        download = mode.startswith("download")
        host = "legacy" if mode.endswith("legacy") else "fixture"
        source_base = (self.home if download else self.local) / mode / "input"
        target_base = (self.home if upload else self.root) / mode / "output"
        source_base.mkdir(parents=True)
        target_base.mkdir(parents=True)

        def source(path):
            return f"{host}:{path}" if download else str(path)

        def target(path, slash=False):
            spelling = str(path) + ("/" if slash else "")
            return f"{host}:{spelling}" if upload else spelling

        return source_base, target_base, source, target

    # A trailing slash expresses a parent directory even before it exists.
    def test_single_file_missing_slash_target_creates_parent_keeps_basename(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "report.txt", "report bytes")
                destination = out / "new directory"
                self.assert_ok(self.run_bcmr("copy", source(file), target(destination, slash=True)))
                self.assert_files(out, {"new directory/report.txt": "report bytes"})

    # Full file destinations must create missing ancestor directories.
    def test_single_file_nested_file_target_creates_missing_parents(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "report.txt", "nested bytes")
                self.assert_ok(self.run_bcmr("copy", source(file), target(out / "one/two/renamed.txt")))
                self.assert_files(out, {"one/two/renamed.txt": "nested bytes"})

    def test_missing_source_fails_before_target_writes(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                result = self.run_bcmr("copy", source(src / "missing.txt"), target(out))
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assert_files(out, {})

    def test_recursive_source_trailing_slash_keeps_directory_basename(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                self.put(src, "tree/nested/leaf.txt", "tree bytes")
                self.assert_ok(self.run_bcmr("copy", "-r", source(str(src / "tree") + "/"), target(out)))
                self.assert_files(out, {"tree/nested/leaf.txt": "tree bytes"})

    # Without the trailing slash, one file may be renamed to a missing target.
    def test_single_file_missing_bare_target_is_a_rename(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "report.txt", "rename bytes")
                self.assert_ok(self.run_bcmr("copy", source(file), target(out / "renamed")))
                self.assert_files(out, {"renamed": "rename bytes"})

    # Existing destination directories are parents even without a trailing slash.
    def test_single_file_existing_directory_keeps_basename(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "report.txt", "existing parent")
                self.assert_ok(self.run_bcmr("copy", source(file), target(out)))
                self.assert_files(out, {"report.txt": "existing parent"})

    # Destination inference must not bypass the explicit source recursion gate.
    def test_directory_source_requires_recursive_before_any_target_write(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                self.put(src, "tree/leaf.txt", "tree bytes")
                result = self.run_bcmr("copy", source(src / "tree"), target(out / "missing", slash=True))
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertRegex(result.stderr.lower(), r"recursive|\-r")
                self.assertEqual(list(out.iterdir()), [])

    # A missing bare target names the copied tree's root, matching common cp.
    def test_recursive_directory_missing_bare_target_names_new_root(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                self.put(src, "tree/nested/leaf.txt", "tree bytes")
                self.assert_ok(self.run_bcmr("copy", "-r", source(src / "tree"), target(out / "renamed")))
                self.assert_files(out, {"renamed/nested/leaf.txt": "tree bytes"})

    # An existing parent directory must nest the source tree's basename.
    def test_recursive_directory_existing_parent_nests_basename(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                self.put(src, "tree/nested/leaf.txt", "tree bytes")
                self.assert_ok(self.run_bcmr("copy", "-r", source(src / "tree"), target(out)))
                self.assert_files(out, {"tree/nested/leaf.txt": "tree bytes"})

    # The slash means parent-directory intent for directory sources as well.
    def test_recursive_directory_missing_slash_target_nests_basename(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                self.put(src, "tree/nested/leaf.txt", "tree bytes")
                self.assert_ok(self.run_bcmr("copy", "-r", source(src / "tree"), target(out / "new", slash=True)))
                self.assert_files(out, {"new/tree/nested/leaf.txt": "tree bytes"})

    # Explicit new parent directories must support several independent sources.
    def test_multiple_files_missing_slash_target_creates_directory(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                one = self.put(src, "one.txt", "one")
                two = self.put(src, "two.txt", "two")
                self.assert_ok(self.run_bcmr("copy", source(one), source(two), target(out / "new", slash=True)))
                self.assert_files(out, {"new/one.txt": "one", "new/two.txt": "two"})

    # A missing bare target is ambiguous for several sources; never overwrite it.
    def test_multiple_files_missing_bare_target_rejected_before_writes(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                one = self.put(src, "one.txt", "one")
                two = self.put(src, "two.txt", "two")
                result = self.run_bcmr("copy", source(one), source(two), target(out / "new"))
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertRegex(result.stderr.lower(), r"director|multiple|sources")
                self.assertEqual(list(out.iterdir()), [])

    # Dry-run planning must predict the same file target without creating dirs.
    def test_dry_run_missing_slash_target_shows_basename_without_creating_target(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "report.txt", "dry bytes")
                destination = out / "new"
                result = self.run_bcmr("copy", "--dry-run", source(file), target(destination, slash=True))
                self.assert_ok(result)
                self.assertIn(str(destination / "report.txt"), result.stdout + result.stderr)
                self.assertEqual(list(out.iterdir()), [])

    def test_recursive_dry_run_does_not_create_destination_directories(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                self.put(src, "tree/nested/leaf.txt", "preview")
                result = self.run_bcmr("copy", "-r", "--dry-run", source(src / "tree"),
                                       target(out / "new", slash=True))
                self.assert_ok(result)
                self.assertEqual(list(out.iterdir()), [])

    def test_upload_dot_destination_keeps_basename(self):
        expected = {}
        for mode in ("upload-serve", "upload-legacy"):
            with self.subTest(mode=mode):
                name = mode + ".txt"
                source = self.put(self.local, name, "home bytes")
                host = "legacy" if mode.endswith("legacy") else "fixture"
                self.assert_ok(self.run_bcmr("copy", source, host + ":."))
                expected[name] = "home bytes"
                self.assert_files(self.home, expected)

    # A slash cannot silently turn an existing regular file into a valid target.
    def test_existing_file_with_trailing_slash_rejected_and_preserved(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                file = self.put(src, "report.txt", "new bytes")
                blocker = self.put(out, "blocker", "original bytes")
                result = self.run_bcmr("copy", "-f", "-y", source(file), target(blocker, slash=True))
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assert_files(out, {"blocker": "original bytes"})


if __name__ == "__main__":
    unittest.main(verbosity=2)
