#!/usr/bin/env python3
"""Remote move cleanup preserves changed sources and removes captured hardlinks."""
import os
import shlex
import unittest

import star_copy_test as fixture
import remote_overwrite_test as overwrite_fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX shell and links")
class RemoteCleanupTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    inject_transport_before_exec = overwrite_fixture.RemoteOverwriteTests.inject_transport_before_exec

    def test_remote_tree_move_removes_all_captured_hardlinks(self):
        for host in ("legacy", "fixture"):
            with self.subTest(host=host):
                tree = self.home / f"{host}-hardlink-tree"
                first = self.put(tree, "a", "captured hardlink bytes")
                second = tree / "b"
                os.link(first, second)
                third = tree / "nested/c"
                third.parent.mkdir()
                os.link(first, third)
                self.assertEqual(first.stat().st_ino, second.stat().st_ino)
                source = tree.name if host == "fixture" else tree
                result = self.run_bcmr("move", "-r", f"{host}:{source}", self.dest)
                self.assert_ok(result)
                self.assertFalse(tree.exists(), "cleanup retained a captured hardlink alias")
                for name in ("a", "b", "nested/c"):
                    self.assertEqual((self.dest / tree.name / name).read_text(),
                                     "captured hardlink bytes")

    def test_remote_fanout_move_removes_all_captured_hardlinks(self):
        tree = self.home / "hardlink-tree"
        first = self.put(tree, "a", "captured hardlink bytes")
        second = tree / "b"
        os.link(first, second)
        other = self.root / "other-destination"
        other.mkdir()
        result = self.run_bcmr("move", "-r", f"legacy:{tree}",
                               "--to", self.dest, "--to", other)
        self.assert_ok(result)
        self.assertFalse(tree.exists(), "fan-out cleanup retained a captured hardlink alias")
        for destination in (self.dest, other):
            for name in ("a", "b"):
                self.assertEqual((destination / "hardlink-tree" / name).read_text(),
                                 "captured hardlink bytes")

    def test_remote_distinct_inode_tree_move_control(self):
        tree = self.home / "distinct-tree"
        first = self.put(tree, "a", "captured hardlink bytes")
        second = self.put(tree, "b", "captured hardlink bytes")
        self.assertNotEqual(first.stat().st_ino, second.stat().st_ino)
        result = self.run_bcmr("move", "-r", f"legacy:{tree}", self.dest)
        self.assert_ok(result)
        self.assertFalse(tree.exists())
        for name in ("a", "b"):
            self.assertEqual((self.dest / "distinct-tree" / name).read_text(),
                             "captured hardlink bytes")

    def test_remote_in_place_change_with_restored_mtime_is_preserved(self):
        source = self.put(self.home, "source", "original bytes")
        stamp = source.stat()
        marker = self.root / "mutated-once"
        mutation = "python3 -c " + shlex.quote(
            'import os; path = %r; stream = open(path, "r+b"); '
            'stream.write(b"modified bytes"); stream.close(); '
            'os.utime(path, ns=(%d, %d))' %
            (str(source), stamp.st_atime_ns, stamp.st_mtime_ns))
        self.inject_transport_before_exec('''
command = ' '.join(words)
if command.startswith("cat '") and %r in command and not os.path.exists(%r):
    open(%r, 'w').close()
    words = [command + '; ' + %r]
''' % (str(source), str(marker), str(marker), mutation))
        result = self.run_bcmr("move", f"legacy:{source}", self.dest / "copied")
        self.assertTrue(marker.exists(), "in-place mutation injection was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(source.read_text(), "modified bytes")
        after = source.stat()
        self.assertEqual((after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns),
                         (stamp.st_dev, stamp.st_ino, stamp.st_size, stamp.st_mtime_ns))
        self.assertNotEqual(after.st_ctime_ns, stamp.st_ctime_ns)
        self.assertEqual((self.dest / "copied").read_text(), "original bytes")

    def test_remote_hardlink_change_after_preceding_cleanup_is_preserved(self):
        tree = self.home / "hardlink-tree"
        first = self.put(tree, "a", "original bytes")
        second = tree / "b"
        os.link(first, second)
        marker = self.root / "mutated-after-first-cleanup"
        self.inject_transport_before_exec('''
import shlex
command = shlex.split(' '.join(words))
if len(command) > 5 and command[3] == 'bcmr-cleanup-leaf' and command[5] == 'b':
    assert not os.path.exists(%r), 'preceding hardlink alias was not removed'
    stamp = os.stat(%r)
    with open(%r, 'r+b') as stream:
        stream.write(b'modified bytes')
    os.utime(%r, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
    assert os.stat(%r).st_ctime_ns != stamp.st_ctime_ns, 'fixture did not advance ctime'
    open(%r, 'w').close()
''' % (str(first), str(second), str(second), str(second), str(second), str(marker)))
        result = self.run_bcmr("move", "-r", f"legacy:{tree}", self.dest)
        self.assertTrue(marker.exists(), "post-cleanup mutation injection was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(first.exists())
        self.assertEqual(second.read_text(), "modified bytes")
        for name in ("a", "b"):
            self.assertEqual((self.dest / "hardlink-tree" / name).read_text(), "original bytes")

    def test_remote_directory_cleanup_preserves_a_late_file(self):
        tree = self.home / "tree"
        source = self.put(tree, "selected", "captured bytes")
        late = tree / "late-arrival"
        self.inject_transport_before_exec('''
command = ' '.join(words)
if command.startswith("cat '") and %r in command:
    with open(%r, 'w') as stream:
        stream.write('late bytes')
''' % (str(source), str(late)))
        result = self.run_bcmr("move", "-r", f"legacy:{tree}", self.dest)
        self.assert_ok(result)
        self.assertTrue(late.is_file(), "cleanup deleted content absent from captured inventory")
        self.assertEqual(late.read_text(), "late bytes")
        self.assertFalse(source.exists())
        self.assertEqual((self.dest / "tree/selected").read_text(), "captured bytes")

    def test_remote_replacement_inode_is_preserved_even_with_identical_bytes(self):
        source = self.put(self.home, "source", "same content")
        replacement = self.home / "replacement"
        marker = self.root / "replaced-once"
        mutation = "printf '%s' 'same content' > {} && mv -f {} {}".format(
            shlex.quote(str(replacement)), shlex.quote(str(replacement)), shlex.quote(str(source)))
        self.inject_transport_before_exec('''
command = ' '.join(words)
if command.startswith("cat '") and %r in command and not os.path.exists(%r):
    with open(%r, 'w') as stream:
        stream.write('once')
    words = [command + '; ' + %r]
''' % (str(source), str(marker), str(marker), mutation))
        result = self.run_bcmr("move", f"legacy:{source}", self.dest / "copied")
        self.assertTrue(marker.exists(), "source replacement injection was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(source.is_file(), "cleanup deleted the replacement inode")
        self.assertEqual(source.read_text(), "same content")


if __name__ == "__main__":
    unittest.main(verbosity=2)
