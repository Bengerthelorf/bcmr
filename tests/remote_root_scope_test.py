#!/usr/bin/env python3
"""SSH copies use login permissions; explicit serve roots remain confined.

Only the SSH process boundary is replaced. CLI planning, shell commands,
serve framing, symlink resolution and filesystem I/O use the real executable.
BCMR=/absolute/path/to/bcmr python3 tests/remote_root_scope_test.py
"""
import json
import os
import struct
import subprocess
import unittest

import star_copy_test as fixture
from serve_stream_lifecycle_test import frame, string


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem and shell")
class RemoteRootScopeTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    def run_check(self, *paths):
        return subprocess.run([fixture.BCMR, "check", "--json", *map(str, paths)],
                              env=self.env, cwd=self.local, text=True,
                              capture_output=True, timeout=30)

    def check_report(self, result):
        # Progress events precede the final (possibly pretty-printed) result.
        decoder = json.JSONDecoder()
        remaining = result.stdout.lstrip()
        reports = []
        while remaining:
            value, end = decoder.raw_decode(remaining)
            if isinstance(value, dict) and "in_sync" in value:
                reports.append(value)
            remaining = remaining[end:].lstrip()
        self.assertEqual(len(reports), 1, result.stdout)
        return reports[0]

    # Catches applying the default home jail to ordinary SSH downloads.
    def test_download_absolute_file_outside_remote_home(self):
        source = self.put(self.root, "external disk/manuscript/main.pdf", "outside-home bytes")
        landed = self.dest / "main.pdf"
        self.assert_ok(self.run_bcmr("copy", f"fixture:{source}", landed))
        self.assertEqual(landed.read_text(), "outside-home bytes")

    # The apparent home prefix must not hide the external referent from this test.
    def test_download_through_home_symlink_to_external_directory(self):
        external = self.root / "external disk"
        self.put(external, "manuscript/main.pdf", "followed external bytes")
        link = self.home / "project"
        link.symlink_to(external, target_is_directory=True)
        for index, remote_path in enumerate((str(link / "manuscript/main.pdf"),
                                              "project/manuscript/main.pdf",
                                              "~/project/manuscript/main.pdf")):
            with self.subTest(remote_path=remote_path):
                landed = self.dest / f"download-{index}.pdf"
                self.assert_ok(self.run_bcmr("copy", f"fixture:{remote_path}", landed))
                self.assertEqual(landed.read_text(), "followed external bytes")
        self.assertTrue(link.is_symlink())

    def test_upload_outside_home_directly_and_through_directory_symlink(self):
        source = self.put(self.local, "source.txt", "uploaded external bytes")
        external = self.root / "external disk"
        external.mkdir()
        link = self.home / "disk"
        link.symlink_to(external, target_is_directory=True)
        for index, remote_base in enumerate((str(external), str(link), "disk", "~/disk")):
            with self.subTest(remote_base=remote_base):
                remote_path = f"{remote_base}/new-{index}/payload.txt"
                self.assert_ok(self.run_bcmr("copy", source, f"fixture:{remote_path}"))
                self.assertEqual((external / f"new-{index}/payload.txt").read_text(),
                                 "uploaded external bytes")
        self.assertTrue(link.is_symlink())

    def test_recursive_download_follows_home_symlink_outside_home(self):
        external = self.root / "external tree"
        self.put(external, "nested/payload.txt", "recursive external bytes")
        (external / "empty").mkdir()
        (self.home / "project").symlink_to(external, target_is_directory=True)
        self.assert_ok(self.run_bcmr("copy", "-r", "fixture:project", self.dest))
        self.assertEqual((self.dest / "project/nested/payload.txt").read_text(),
                         "recursive external bytes")
        self.assertTrue((self.dest / "project/empty").is_dir())

    def test_recursive_relative_upload_pipelines_files_to_external_symlink(self):
        tree = self.local / "tree"
        self.put(tree, "one.txt", "first pipelined upload")
        self.put(tree, "nested/two.txt", "second pipelined upload")
        external = self.root / "external disk"
        external.mkdir()
        (self.home / "disk").symlink_to(external, target_is_directory=True)
        self.assert_ok(self.run_bcmr("copy", "-r", "-P", "2", tree, "fixture:disk/"))
        self.assertEqual((external / "tree/one.txt").read_text(), "first pipelined upload")
        self.assertEqual((external / "tree/nested/two.txt").read_text(), "second pipelined upload")

    # Simply changing serve's root to '/' would resolve these under '/' instead.
    def test_relative_and_tilde_file_paths_still_use_remote_home(self):
        self.put(self.home, "payload.txt", "remote home bytes")
        self.put(self.local, "payload.txt", "client cwd decoy")
        for index, remote_path in enumerate(("payload.txt", "./payload.txt", "~/payload.txt")):
            with self.subTest(remote_path=remote_path):
                landed = self.dest / f"relative-{index}.txt"
                self.assert_ok(self.run_bcmr("copy", f"fixture:{remote_path}", landed))
                self.assertEqual(landed.read_text(), "remote home bytes")
        source = self.put(self.local, "upload.txt", "relative upload bytes")
        self.assert_ok(self.run_bcmr("copy", source, "fixture:new/nested/upload.txt"))
        self.assertEqual((self.home / "new/nested/upload.txt").read_text(),
                         "relative upload bytes")

    # Trimming the HOME probe silently redirects relative paths to the sibling.
    def test_relative_download_preserves_trailing_whitespace_in_remote_home(self):
        for index, suffix in enumerate((" ", "\n")):
            with self.subTest(home_suffix=repr(suffix)):
                base = self.root / f"remote-home-{index}"
                home = self.root / (base.name + suffix)
                self.put(home, "payload.txt", "real home bytes")
                self.put(base, "payload.txt", "fake home bytes")
                destination = self.dest / f"whitespace-home-{index}.txt"
                env = dict(self.env, STAR_FIXTURE_HOME=str(home))
                self.assert_ok(self.run_bcmr("copy", "fixture:payload.txt", destination,
                                             env=env))
                self.assertEqual(destination.read_text(), "real home bytes")

    # Preserve both login-directory anchoring and the contents-copy convention.
    def test_empty_and_dot_remote_source_copy_home_contents(self):
        self.put(self.home, "payload.txt", "home directory contents")
        for index, remote_path in enumerate(("fixture:", "fixture:.", "fixture:./")):
            with self.subTest(remote_path=remote_path):
                destination = self.dest / str(index)
                destination.mkdir()
                self.assert_ok(self.run_bcmr("copy", "-r", remote_path, destination))
                self.assertEqual((destination / "payload.txt").read_text(),
                                 "home directory contents")
                self.assertFalse((destination / self.home.name).exists())

    def test_empty_and_dot_remote_destination_receive_source_basename(self):
        for index, remote_path in enumerate(("fixture:", "fixture:.", "fixture:./")):
            with self.subTest(remote_path=remote_path):
                source = self.put(self.local, f"upload-{index}.txt", "home destination bytes")
                self.assert_ok(self.run_bcmr("copy", source, remote_path))
                self.assertEqual((self.home / source.name).read_text(), "home destination bytes")

    # Check uses STAT and HASH independently of the copy planner. Each spelling
    # must compare the remote file, in either source or destination position.
    def test_check_relative_absolute_and_symlink_files_against_local(self):
        local = self.put(self.local, "payload.txt", "identical check bytes")
        self.put(self.home, "payload.txt", "identical check bytes")
        external = self.put(self.root, "external/payload.txt", "identical check bytes")
        (self.home / "disk").symlink_to(external.parent, target_is_directory=True)
        os.utime(local, (1000, 1000))
        for remote_path in ("payload.txt", str(external), "disk/payload.txt"):
            for remote_source in (False, True):
                with self.subTest(remote_path=remote_path, remote_source=remote_source):
                    remote = f"fixture:{remote_path}"
                    paths = (remote, local) if remote_source else (local, remote)
                    result = self.run_check(*paths)
                    self.assert_ok(result)
                    self.assertTrue(self.check_report(result)["in_sync"])

    # Drifted mtimes force HASH after LIST; a same-size edit is the negative
    # control so a skipped/misdirected comparison cannot pass as in sync.
    def test_recursive_check_relative_tree_lists_and_hashes_remote_entries(self):
        remote = self.put(self.home, "tree/nested/payload.txt", "matching tree bytes")
        local = self.put(self.local, "tree/nested/payload.txt", "matching tree bytes")
        os.utime(remote, (2000, 2000))
        os.utime(local, (1000, 1000))
        result = self.run_check("-r", "fixture:tree", self.local)
        self.assert_ok(result)
        self.assertTrue(self.check_report(result)["in_sync"])
        local.write_text("modified tree bytes")
        os.utime(local, (1000, 1000))
        result = self.run_check("-r", "fixture:tree", self.local)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        report = self.check_report(result)
        self.assertFalse(report["in_sync"])
        self.assertEqual(report["summary"]["modified"], 1)

    # A direct channel must retain the SSH session's relative-path base.
    def test_direct_tcp_relative_upload_and_download_use_remote_home(self):
        source = self.put(self.local, "payload.txt", "direct channel bytes")
        env = dict(self.env, SSH_CONNECTION="127.0.0.1 40000 127.0.0.1 22")
        self.assert_ok(self.run_bcmr("copy", "--direct=direct", source,
                                     "fixture:received.txt", env=env))
        self.assertEqual((self.home / "received.txt").read_text(), "direct channel bytes")
        destination = self.dest / "download.txt"
        self.assert_ok(self.run_bcmr("copy", "--direct=direct", "fixture:received.txt",
                                     destination, env=env))
        self.assertEqual(destination.read_text(), "direct channel bytes")

    # Resume independently sends STAT, HASH, and offset PUT for the same name.
    def test_relative_upload_resume_validates_and_extends_existing_prefix(self):
        prefix = "validated prefix\n" * 4096
        contents = prefix + "uploaded suffix\n" * 4096
        source = self.put(self.local, "source.txt", contents)
        destination = self.put(self.home, "partial.txt", prefix)
        self.assert_ok(self.run_bcmr("copy", "--resume", "-V", source,
                                     "fixture:partial.txt"))
        self.assertEqual(destination.read_text(), contents)

    @unittest.skipIf(not hasattr(os, "geteuid") or os.geteuid() == 0,
                     "requires a non-root POSIX user for mode-bit read denial")
    def test_unreadable_external_source_still_fails_without_publishing(self):
        source = self.put(self.root, "external/private.txt", "must not be copied")
        source.chmod(0)
        self.addCleanup(source.chmod, 0o600)
        destination = self.dest / "private.txt"
        result = self.run_bcmr("copy", f"fixture:{source}", destination)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(destination.exists())
        self.assertRegex(result.stderr.lower(), r"permission denied|not permitted")

    @unittest.skipIf(not hasattr(os, "geteuid") or os.geteuid() == 0,
                     "requires a non-root POSIX user for mode-bit write denial")
    def test_unwritable_external_directory_still_rejects_upload(self):
        source = self.put(self.local, "source.txt", "must not be uploaded")
        directory = self.root / "external-private"
        directory.mkdir()
        directory.chmod(0)
        self.addCleanup(directory.chmod, 0o700)
        result = self.run_bcmr("copy", source, f"fixture:{directory}/payload.txt")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stderr.lower(), r"permission denied|not permitted")
        directory.chmod(0o700)
        self.assertEqual(list(directory.iterdir()), [])

    # A configured jail must still reject physical and symlink escapes. The
    # allowed relative STAT also proves the real protocol and jail base work.
    def test_explicit_serve_root_keeps_relative_base_and_rejects_escapes(self):
        self.put(self.home, "allowed.txt", "allowed")
        outside = self.put(self.root, "external/secret.txt", "outside secret")
        (self.home / "escape").symlink_to(outside)
        requests = [b"\x03" + string(path)
                    for path in ("allowed.txt", outside, "escape")]
        wire = frame(b"\x01\x02\x00") + b"".join(map(frame, requests))
        result = subprocess.run([fixture.BCMR, "serve", "--root", str(self.home)],
                                input=wire, env=self.env, cwd=self.local,
                                capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        replies = []
        remaining = result.stdout
        while remaining:
            self.assertGreaterEqual(len(remaining), 4)
            length = struct.unpack("<I", remaining[:4])[0]
            self.assertGreater(length, 0)
            self.assertLessEqual(length, len(remaining) - 4)
            replies.append(remaining[4:4 + length])
            remaining = remaining[4 + length:]
        self.assertEqual(len(replies), 4)
        self.assertEqual(replies[0], b"\x81\x02\x00")
        self.assertEqual(replies[1][0], 0x85)
        self.assertEqual(struct.unpack("<Q", replies[1][1:9])[0], 7)
        for reply in replies[2:]:
            self.assertEqual(reply[0], 0x83)
            self.assertIn(b"escapes server root", reply)


if __name__ == "__main__":
    unittest.main(verbosity=2)
