#!/usr/bin/env python3
"""Everyday local copy/move contracts against the real CLI and filesystem.

Run with BCMR=/absolute/path/to/bcmr python3 tests/local_copy_move_ux_test.py.
"""
import os
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem links")
class LocalCopyMoveUxTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    assert_files = fixture.SourceStarTests.assert_files

    # Self-copy must not atomically replace the source or claim to have copied it.
    def test_copy_to_same_file_refuses_before_any_source_mutation(self):
        for options in (("-f", "-y"), ("-f",), ("--dry-run",)):
            with self.subTest(options=options):
                source = self.put(self.local, "source.txt", "keep this source")
                source.chmod(0o640)
                os.utime(source, ns=(1_600_000_000_000_000_000,) * 2)
                before = source.stat()
                result = self.run_bcmr("copy", *options, source, source)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("same file", result.stderr.lower())
                after = source.stat()
                self.assertEqual((after.st_ino, after.st_mode, after.st_mtime_ns),
                                 (before.st_ino, before.st_mode, before.st_mtime_ns))
                self.assertEqual(source.read_text(), "keep this source")

    # Different names for one inode are still the same file.
    def test_copy_to_hardlink_alias_refuses_without_breaking_the_link(self):
        source = self.put(self.local, "source.txt", "one inode")
        destination = self.dest / "alias.txt"
        os.link(source, destination)
        before = source.stat()
        result = self.run_bcmr("copy", "-f", "-y", source, destination)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("same file", result.stderr.lower())
        self.assertEqual(source.stat().st_ino, before.st_ino)
        self.assertEqual(destination.stat().st_ino, before.st_ino)
        self.assertEqual(source.read_text(), "one inode")

    # SSH aliases of this machine must not replace a source through its other name.
    def test_loopback_transfer_rejects_same_file_and_hardlink_before_mutation(self):
        for operation in ("copy", "move"):
            for direction in ("upload", "download"):
                for alias in ("same-path", "hardlink"):
                    with self.subTest(operation=operation, direction=direction, alias=alias):
                        case = f"{operation}-{direction}-{alias}"
                        source = self.put(self.local, case + "/source", "source bytes")
                        destination = source if alias == "same-path" else source.with_name("alias")
                        if alias == "hardlink":
                            os.link(source, destination)
                        before = source.stat()
                        selected = str(source)
                        target = str(destination)
                        if direction == "upload":
                            target = "fixture:" + target
                        else:
                            selected = "fixture:" + selected
                        result = self.run_bcmr(operation, "-f", "-y", selected, target)
                        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn("same file", result.stderr.lower())
                        self.assertEqual(source.stat().st_ino, before.st_ino)
                        self.assertEqual(destination.stat().st_ino, before.st_ino)
                        self.assertEqual(source.read_text(), "source bytes")

    def test_loopback_resume_rejects_same_file_before_treating_it_as_complete(self):
        for direction in ("upload", "download"):
            with self.subTest(direction=direction):
                source = self.put(self.local, direction + "-resume", "already complete")
                before = source.stat()
                selected = str(source)
                target = str(source)
                if direction == "upload":
                    target = "fixture:" + target
                else:
                    selected = "fixture:" + selected
                result = self.run_bcmr("copy", "--resume", selected, target)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("same file", result.stderr.lower())
                self.assertEqual(source.stat().st_ino, before.st_ino)
                self.assertEqual(source.read_text(), "already complete")

    # Every selected leaf must be checked before an earlier, distinct file is written.
    def test_recursive_loopback_alias_rejection_precedes_all_writes(self):
        for direction in ("upload", "download"):
            with self.subTest(direction=direction):
                source = self.local / (direction + "-source") / "tree"
                parent = self.home / (direction + "-destination")
                destination = parent / "tree"
                self.put(source, "a-first", "new bytes")
                alias_source = self.put(source, "z-alias", "same inode")
                old = self.put(destination, "a-first", "old bytes")
                alias_target = destination / "z-alias"
                os.link(alias_source, alias_target)
                before = (old.stat().st_ino, alias_source.stat().st_ino)
                selected = str(source)
                target = str(parent)
                if direction == "upload":
                    target = "fixture:" + target
                else:
                    selected = "fixture:" + selected
                result = self.run_bcmr("copy", "-r", "-f", "-y", selected, target)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("same file", result.stderr.lower())
                self.assertEqual(old.read_text(), "old bytes")
                self.assertEqual((old.stat().st_ino, alias_source.stat().st_ino), before)
                self.assertEqual(alias_target.stat().st_ino, before[1])

    def test_distinct_remote_file_with_identical_contents_is_not_a_self_copy(self):
        source = self.put(self.local, "same-name", "identical content")
        destination = self.put(self.second, "same-name", "identical content")
        source_inode = source.stat().st_ino
        self.assert_ok(self.run_bcmr("copy", "-f", "-y", source,
                                     "second:" + str(destination)))
        self.assertEqual(source.stat().st_ino, source_inode)
        self.assertEqual(destination.read_text(), "identical content")

    # Devices/inodes can coincide on different machines; the host proof is required.
    def test_identical_file_ids_in_a_different_boot_namespace_do_not_reject_copy(self):
        source = self.put(self.local, "payload", "new content")
        destination = self.put(self.second, "payload", "old content")
        self.env["UX_IDENTITY_SOURCE"] = str(source)
        transport = self.bin / "ssh"
        transport.write_text(transport.read_text().replace(
            "os.execv('/bin/sh',", """if host == 'second' and 'bcmr-relay-identity' in ' '.join(words):
    import subprocess
    response = subprocess.run(['/bin/sh', '-c', ' '.join(words)], capture_output=True)
    if response.returncode:
        sys.stderr.buffer.write(response.stderr)
        sys.exit(response.returncode)
    fields = response.stdout.split(b'\\0')
    assert len(fields) == 7
    source = os.environ['UX_IDENTITY_SOURCE']
    metadata = os.stat(source)
    fields[0] = fields[1] = os.fsencode(os.path.realpath(source))
    fields[2] = b'different-host-boot-uuid'
    fields[4] = fields[5] = f'{metadata.st_dev}:{metadata.st_ino}'.encode()
    sys.stdout.buffer.write(b'\\0'.join(fields))
    sys.exit(0)
os.execv('/bin/sh',""", 1))
        source_inode = source.stat().st_ino
        self.assert_ok(self.run_bcmr("copy", "-f", "-y", source,
                                     "second:" + str(destination)))
        self.assertEqual(source.stat().st_ino, source_inode)
        self.assertEqual(source.read_text(), "new content")
        self.assertEqual(destination.read_text(), "new content")

    def test_preserved_symlink_self_copy_refuses_without_replacing_the_link(self):
        source = self.local / "dangling"
        source.symlink_to("missing target")
        before = source.lstat()
        result = self.run_bcmr("copy", "--no-deref", "-f", "-y", source, source)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("same file", result.stderr.lower())
        self.assertEqual(source.lstat().st_ino, before.st_ino)
        self.assertEqual(os.readlink(source), "missing target")

    def test_loopback_preserved_symlink_self_copy_refuses_before_mutation(self):
        for direction in ("upload", "download"):
            with self.subTest(direction=direction):
                source = self.local / (direction + "-link")
                source.symlink_to("missing target")
                before = source.lstat().st_ino
                selected = str(source)
                target = str(source)
                if direction == "upload":
                    target = "fixture:" + target
                else:
                    selected = "fixture:" + selected
                result = self.run_bcmr("copy", "--no-deref", "-f", "-y", selected, target)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("same file", result.stderr.lower())
                self.assertEqual(source.lstat().st_ino, before)
                self.assertEqual(os.readlink(source), "missing target")

    def test_composite_copy_rejects_local_hardlink_alias_before_any_writes(self):
        source = self.put(self.local, "payload", "one inode")
        destination = self.dest / "payload"
        os.link(source, destination)
        self.put(self.home, "unrelated", "remote bytes")
        before = source.stat().st_ino
        result = self.run_bcmr("copy", "-f", "-y", "fixture:unrelated", source, self.dest)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("same file", result.stderr.lower())
        self.assertEqual(source.stat().st_ino, before)
        self.assertEqual(destination.stat().st_ino, before)
        self.assertFalse((self.dest / "unrelated").exists())
        self.assertEqual(source.read_text(), "one inode")

    def test_distinct_preserved_links_sharing_referent_copy_over_ssh_and_composite(self):
        referent = self.local / "shared-missing-referent"
        for route in ("upload", "download", "composite"):
            with self.subTest(route=route):
                source = self.local / (route + "-link")
                destination = self.dest / source.name
                source.symlink_to(referent)
                destination.symlink_to(referent)
                selected = str(source)
                target = str(destination)
                extra = []
                if route == "upload":
                    target = "fixture:" + target
                elif route == "download":
                    selected = "fixture:" + selected
                else:
                    self.put(self.home, "unrelated", "remote bytes")
                    extra = ["fixture:unrelated"]
                    target = str(self.dest)
                self.assert_ok(self.run_bcmr("copy", "--no-deref", "-f", "-y",
                                             selected, *extra, target))
                self.assertEqual(os.readlink(source), str(referent))
                self.assertEqual(os.readlink(destination), str(referent))
                self.assertFalse(referent.exists())

    # Two separate preserved links remain distinct objects, even with one target.
    def test_distinct_preserved_symlinks_to_one_target_can_be_copied(self):
        target = self.put(self.local, "target", "referent")
        source = self.local / "first-link"
        destination = self.dest / "second-link"
        source.symlink_to(target)
        destination.symlink_to(target)
        self.assert_ok(self.run_bcmr("copy", "--no-deref", "-f", "-y",
                                     source, destination))
        self.assertTrue(source.is_symlink())
        self.assertTrue(destination.is_symlink())
        self.assertEqual(os.readlink(destination), str(target))
        self.assertEqual(target.read_text(), "referent")

    # Explicit '.' copies directory contents, including hidden names.
    def test_current_directory_contents_copy_to_existing_or_missing_destination(self):
        self.put(self.local, "visible.txt", "visible")
        self.put(self.local, ".hidden", "hidden")
        self.put(self.local, "nested/leaf.txt", "nested")
        expected = {"visible.txt": "visible", ".hidden": "hidden",
                    "nested/leaf.txt": "nested"}
        for source, existing in ((".", True), ("./", False)):
            with self.subTest(source=source, existing=existing):
                destination = self.dest / ("existing" if existing else "missing")
                if existing:
                    destination.mkdir()
                self.assert_ok(self.run_bcmr("copy", "-r", source, destination))
                self.assert_files(destination, expected)

    def test_explicit_directory_dot_copies_contents_without_extra_container(self):
        source = self.local / "tree"
        self.put(source, "leaf.txt", "contents")
        self.assert_ok(self.run_bcmr("copy", "-r", str(source) + "/.", self.dest))
        self.assert_files(self.dest, {"leaf.txt": "contents"})

    def test_plain_directory_trailing_slash_still_keeps_its_basename(self):
        source = self.local / "tree"
        self.put(source, "leaf.txt", "contents")
        self.assert_ok(self.run_bcmr("copy", "-r", str(source) + "/", self.dest))
        self.assert_files(self.dest, {"tree/leaf.txt": "contents"})

    # Upload and download must interpret an explicit final dot the same way.
    def test_remote_copy_explicit_dot_and_plain_slash_choose_the_same_targets(self):
        for direction in ("upload", "download"):
            for suffix in ("/.", "/"):
                with self.subTest(direction=direction, suffix=suffix):
                    case = direction + ("-dot" if suffix == "/." else "-slash")
                    source_base = self.local if direction == "upload" else self.home
                    target_base = self.home if direction == "upload" else self.dest
                    source = source_base / case / "tree"
                    destination = target_base / case / "out"
                    self.put(source, "leaf.txt", "contents")
                    destination.mkdir(parents=True)
                    selected = str(source) + suffix
                    target = str(destination)
                    if direction == "upload":
                        target = "fixture:" + target
                    else:
                        selected = "fixture:" + selected
                    self.assert_ok(self.run_bcmr("copy", "-r", selected, target))
                    wanted = "leaf.txt" if suffix == "/." else "tree/leaf.txt"
                    self.assert_files(destination, {wanted: "contents"})

    # Previewing an overwrite needs no force or confirmation and never mutates.
    def test_move_dry_run_previews_existing_target_without_requiring_force(self):
        for kind in ("file", "link"):
            with self.subTest(kind=kind):
                source = self.local / kind
                if kind == "file":
                    source.write_text("new bytes")
                else:
                    source.symlink_to("missing target")
                destination = self.put(self.dest, kind, "old bytes")
                before = (source.lstat().st_ino, destination.stat().st_ino)
                result = self.run_bcmr("move", "--dry-run", source, destination)
                self.assert_ok(result)
                self.assertIn("OVERWRITE", result.stdout)
                self.assertEqual((source.lstat().st_ino, destination.stat().st_ino), before)
                self.assertEqual(destination.read_text(), "old bytes")
                if kind == "file":
                    self.assertEqual(source.read_text(), "new bytes")
                else:
                    self.assertEqual(os.readlink(source), "missing target")


if __name__ == "__main__":
    unittest.main(verbosity=2)
