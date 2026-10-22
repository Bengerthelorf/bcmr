#!/usr/bin/env python3
"""Preserved SSH link publication against native mv and the real CLI.

Run with BCMR=/absolute/path/to/bcmr python3 tests/mac_symlink_publish_ux_test.py.
"""
import os
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem links")
class MacSymlinkPublishUxTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    def test_distinct_links_to_one_referent_publish_without_changing_the_referent(self):
        # Make native macOS mv's default conformance behavior explicit. The
        # publication fix must scope its compatibility setting to that command.
        self.env["COMMAND_MODE"] = "unix2003"
        for kind in ("file", "directory", "dangling"):
            with self.subTest(kind=kind):
                referent = self.local / (kind + "-referent")
                payload = None
                if kind == "file":
                    payload = self.put(self.local, referent.name, "keep file payload")
                elif kind == "directory":
                    payload = self.put(referent, "sentinel", "keep directory payload")
                source_parent = self.local / (kind + "-source")
                source_parent.mkdir()
                source = source_parent / "link"
                source.symlink_to(referent, target_is_directory=kind == "directory")
                destination_parent = self.home / (kind + "-destination")
                destination_parent.mkdir()
                destination = destination_parent / "link"
                destination.symlink_to(referent, target_is_directory=kind == "directory")
                source_inode = source.lstat().st_ino
                destination_inode = destination.lstat().st_ino
                payload_before = None if payload is None else (
                    payload.stat().st_ino, payload.stat().st_mtime_ns, payload.read_bytes())

                result = self.run_bcmr("copy", "--no-deref", "-f", "-y", source,
                                       "fixture:" + str(destination_parent) + "/")

                self.assert_ok(result)
                self.assertTrue(destination.is_symlink())
                self.assertEqual(os.readlink(source), str(referent))
                self.assertEqual(os.readlink(destination), str(referent))
                self.assertEqual(source.lstat().st_ino, source_inode)
                self.assertNotEqual(destination.lstat().st_ino, destination_inode)
                if payload is None:
                    self.assertFalse(referent.exists())
                else:
                    self.assertEqual((payload.stat().st_ino, payload.stat().st_mtime_ns,
                                      payload.read_bytes()), payload_before)
                if kind == "directory":
                    self.assertEqual(sorted(path.name for path in referent.iterdir()), ["sentinel"])
                self.assertEqual(sorted(path.name for path in destination_parent.iterdir()), ["link"])

    def test_real_destination_directory_is_not_entered_or_replaced(self):
        referent = self.put(self.local, "referent", "keep source payload")
        source_parent = self.local / "source"
        source_parent.mkdir()
        source = source_parent / "link"
        source.symlink_to(referent)
        destination_parent = self.home / "destination"
        sentinel = self.put(destination_parent / "link", "sentinel", "keep destination payload")
        source_inode = source.lstat().st_ino
        directory_inode = sentinel.parent.stat().st_ino

        result = self.run_bcmr("copy", "--no-deref", "-f", "-y", source,
                               "fixture:" + str(destination_parent) + "/")

        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(source.lstat().st_ino, source_inode)
        self.assertEqual(referent.read_text(), "keep source payload")
        self.assertEqual(sentinel.parent.stat().st_ino, directory_inode)
        self.assertEqual(sentinel.read_text(), "keep destination payload")
        self.assertEqual(sorted(path.name for path in sentinel.parent.iterdir()), ["sentinel"])
        self.assertEqual(sorted(path.name for path in destination_parent.iterdir()), ["link"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
