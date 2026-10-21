#!/usr/bin/env python3
"""Local move contracts; run BCMR=/absolute/path/to/bcmr python3 this-file."""
import os
import pty
import re
import subprocess
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX links and filesystem")
class LocalMoveParityTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    def run_confirmed(self, operation, *args):
        master, slave = pty.openpty()
        try:
            child = subprocess.Popen(
                [fixture.BCMR, operation, "--progress=off", *map(str, args)],
                cwd=self.local, env=self.env, stdin=slave,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            os.close(slave)
            slave = None
            os.write(master, b"y\n")
            stdout, stderr = child.communicate(timeout=30)
            return subprocess.CompletedProcess(child.args, child.returncode, stdout, stderr)
        finally:
            if slave is not None:
                os.close(slave)
            os.close(master)

    def test_self_loop_link_moves_over_an_existing_destination(self):
        source = self.local / "loop"
        source.symlink_to("loop")
        destination = self.put(self.dest, "target", "old destination")
        self.assert_ok(self.run_bcmr("move", "-f", "-y", source, destination))
        self.assertFalse(source.is_symlink())
        self.assertTrue(destination.is_symlink())
        self.assertEqual(os.readlink(destination), "loop")

    def test_distinct_links_to_one_referent_can_replace_each_other(self):
        target = self.put(self.local, "referent", "target bytes")
        source = self.local / "first-link"
        destination = self.dest / "second-link"
        source.symlink_to(target)
        destination.symlink_to(target)
        self.assert_ok(self.run_bcmr("move", "-f", "-y", source, destination))
        self.assertFalse(source.is_symlink())
        self.assertTrue(destination.is_symlink())
        self.assertEqual(os.readlink(destination), str(target))
        self.assertEqual(target.read_text(), "target bytes")

    def test_same_source_link_and_regular_hardlink_moves_are_refused(self):
        target = self.put(self.local, "referent", "target bytes")
        link = self.local / "link"
        link.symlink_to(target)
        alias = self.local / "alias"
        os.link(target, alias)
        for source, destination in ((link, link), (target, alias)):
            with self.subTest(source=source):
                result = self.run_bcmr("move", "-f", "-y", source, destination)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertTrue(source.exists())
                self.assertTrue(destination.exists())
        self.assertTrue(link.is_symlink())
        self.assertEqual(target.read_text(), "target bytes")

    def test_empty_directory_blocker_is_unchanged_on_copy_or_move_failure(self):
        for operation in ("copy", "move"):
            for route in ("plan", "pipeline"):
                for preserve in (False, True):
                    with self.subTest(operation=operation, route=route, preserve=preserve):
                        name = f"{operation}-{route}-{preserve}"
                        source = self.local / name / "tree"
                        (source / "empty").mkdir(parents=True)
                        (source / "empty").chmod(0o751)
                        output = self.dest / name
                        blocker = self.put(output, "tree/empty", "existing file blocker")
                        blocker.chmod(0o640)
                        os.utime(blocker, ns=(1_600_000_000_000_000_000,) * 2)
                        before = blocker.stat()
                        args = ["-r"] + (["-p"] if preserve else [])
                        if route == "plan":
                            result = self.run_confirmed(operation, *args, "-f", source, output)
                        elif operation == "move":
                            result = self.run_bcmr(operation, *args, source, "--to", output)
                        else:
                            result = self.run_bcmr(operation, *args, source, output)
                        after = blocker.stat()
                        self.assertEqual(blocker.read_text(), "existing file blocker")
                        self.assertEqual((after.st_mode, after.st_mtime_ns, after.st_ctime_ns),
                                         (before.st_mode, before.st_mtime_ns, before.st_ctime_ns))
                        self.assertTrue((source / "empty").is_dir(), "failed move removed its source")
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_hardlinked_directory_merge_cleans_every_selected_alias(self):
        source = self.local / "tree"
        first = self.put(source, "first", "hardlinked source bytes")
        (source / "nested").mkdir()
        os.link(first, source / "nested/second")
        (self.dest / "tree").mkdir()
        self.assert_ok(self.run_bcmr("move", "-r", source, self.dest))
        self.assertFalse(source.exists())
        for relative in ("first", "nested/second"):
            self.assertEqual((self.dest / "tree" / relative).read_text(), "hardlinked source bytes")

    # A postorder exclusion filter must not delete descendants pruned at copy.
    def test_exact_directory_exclusion_preserves_uncopied_descendants(self):
        source = self.local / "tree"
        kept = self.put(source, "keep/private.txt", "must remain")
        moved = self.put(source, "move.txt", "selected")
        pattern = re.escape(str(source / "keep")) + "$"
        self.assert_ok(self.run_bcmr("move", "-r", "--exclude", pattern, source, self.dest))
        self.assertTrue(kept.exists(), "cleanup deleted a pruned descendant")
        self.assertEqual(kept.read_text(), "must remain")
        self.assertFalse(moved.exists())
        self.assertEqual((self.dest / "tree/move.txt").read_text(), "selected")
        self.assertFalse((self.dest / "tree/keep").exists())

    # Copying a directory onto itself must never be followed by source cleanup.
    def test_excluded_force_move_into_own_parent_refuses_without_writes(self):
        source = self.local / "tree"
        leaf = self.put(source, "leaf.txt", "keep source")
        result = self.run_bcmr("move", "-r", "-f", "-y", "--exclude", "^never$",
                               source, self.local)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(leaf.read_text(), "keep source")

    def test_excluded_move_into_missing_descendant_refuses_before_mkdir(self):
        source = self.local / "tree"
        leaf = self.put(source, "leaf.txt", "keep source")
        result = self.run_bcmr("move", "-r", "--exclude", "^never$", source,
                               source / "nested")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(leaf.read_text(), "keep source")
        self.assertFalse((source / "nested").exists(), "self-move created a destination inside its input")

    # Reuse the feature-gated copy fault at the actual move publication boundary.
    def test_no_force_move_preserves_a_target_created_before_rename(self):
        source = self.put(self.local, "source.txt", "source bytes")
        destination = self.dest / "target.txt"
        result = self.run_bcmr("move", "--test-mode", "create_destination_before_finalize",
                               source, destination)
        if "Invalid test mode" in result.stderr:
            self.skipTest("requires a test-support binary")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(source.read_text(), "source bytes")
        self.assertEqual(destination.read_bytes(), b"concurrent target")

    # Existing target trees merge independently of same/cross-filesystem rename.
    def test_existing_directory_tree_merges_and_preserves_unrelated_entries(self):
        source = self.local / "tree"
        self.put(source, "nested/new.txt", "new content")
        old = self.put(self.dest, "tree/nested/old.txt", "old content")
        self.assert_ok(self.run_bcmr("move", "-r", source, self.dest))
        self.assertFalse(source.exists())
        self.assertEqual(old.read_text(), "old content")
        self.assertEqual((self.dest / "tree/nested/new.txt").read_text(), "new content")

    # A merge error may publish a destination leaf but must retain every source.
    def test_failed_directory_merge_preserves_all_source_entries(self):
        source = self.local / "tree"
        self.put(source, "first.txt", "first source")
        self.put(source, "blocked.txt", "blocked source")
        blocker = self.put(self.dest, "tree/blocked.txt", "existing destination")
        result = self.run_bcmr("move", "-r", source, self.dest)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual((source / "first.txt").read_text(), "first source")
        self.assertEqual((source / "blocked.txt").read_text(), "blocked source")
        self.assertEqual(blocker.read_text(), "existing destination")

    # The merge copy and cleanup must preserve links and never follow targets.
    def test_directory_merge_preserves_dangling_and_directory_links(self):
        source = self.local / "tree"
        source.mkdir()
        target = self.local / "outside"
        target.mkdir()
        outside = self.put(target, "private.txt", "outside content")
        (source / "directory-link").symlink_to(target, target_is_directory=True)
        (source / "dangling-link").symlink_to("missing-target")
        (self.dest / "tree").mkdir()
        self.assert_ok(self.run_bcmr("move", "-r", source, self.dest))
        self.assertFalse(source.exists())
        self.assertTrue((self.dest / "tree/directory-link").is_symlink())
        self.assertEqual(os.readlink(self.dest / "tree/directory-link"), str(target))
        self.assertTrue((self.dest / "tree/dangling-link").is_symlink())
        self.assertEqual(os.readlink(self.dest / "tree/dangling-link"), "missing-target")
        self.assertEqual(outside.read_text(), "outside content")


if __name__ == "__main__":
    unittest.main(verbosity=2)
