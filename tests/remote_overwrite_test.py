#!/usr/bin/env python3
"""Real CLI overwrite contracts with only SSH transport replaced.

BCMR=/absolute/path/to/bcmr python3 tests/remote_overwrite_test.py
"""
import os
import hashlib
import shlex
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem and shell")
class RemoteOverwriteTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    MODES = ("local", "upload-serve", "upload-legacy", "download-serve", "download-legacy")

    def route(self, mode):
        source = self.put(self.home if mode.startswith("download") else self.local,
                          f"{mode}-source.txt", "new replacement bytes")
        destination = self.put(self.home if mode.startswith("upload") else self.dest,
                               f"{mode}-target.txt", "old destination bytes")
        host = "legacy" if mode.endswith("legacy") else "fixture"
        src_arg = f"{host}:{source}" if mode.startswith("download") else str(source)
        dst_arg = f"{host}:{destination}" if mode.startswith("upload") else str(destination)
        return source, destination, src_arg, dst_arg

    # A transfer must not replace an existing target unless explicitly authorized.
    def test_existing_target_without_force_is_rejected_and_preserved(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                source, destination, src, dst = self.route(mode)
                result = self.run_bcmr("copy", src, dst)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(destination.read_text(), "old destination bytes")
                self.assertEqual(source.read_text(), "new replacement bytes")

    # Force is propagated through both serve and legacy paths.
    def test_force_replaces_existing_target(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                _, destination, src, dst = self.route(mode)
                self.assert_ok(self.run_bcmr("copy", "-f", "-y", src, dst))
                self.assertEqual(destination.read_text(), "new replacement bytes")

    # Explicit resume continues a valid prefix without additionally requiring -f.
    def test_resume_accepts_an_existing_valid_prefix_without_force(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                _, destination, src, dst = self.route(mode)
                destination.write_text("new replacement ")
                self.assert_ok(self.run_bcmr("copy", "-C", src, dst))
                self.assertEqual(destination.read_text(), "new replacement bytes")

    # A failed GET must discard its own stage, never delete the old destination.
    def test_failed_legacy_download_preserves_existing_target(self):
        source, destination, src, dst = self.route("download-legacy")
        injected = self.root / "cat-fault-triggered"
        self.inject_transport_before_exec('''
import shlex
if len(words) == 1 and shlex.split(words[0]) == ['cat', %r]:
    with open(%r, 'w') as marker:
        marker.write('connection interrupted during GET')
    os.execv('/bin/sh', ['sh', '-c', %r])
''' % (str(source), str(injected),
       "head -c 2 " + shlex.quote(str(source)) + "; printf 'fixture connection lost\\n' >&2; exit 9"))
        result = self.run_bcmr("copy", "-f", "-y", src, dst)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(injected.exists(), "GET transfer fault was not reached")
        self.assertTrue(destination.exists(), "failed GET deleted the old destination")
        self.assertEqual(destination.read_text(), "old destination bytes")

    # Cut only the SSH byte stream while a real server sends a large GET body.
    def test_failed_serve_download_preserves_existing_target(self):
        source, destination, src, dst = self.route("download-serve")
        source.write_bytes(os.urandom(1024 * 1024))
        self.inject_transport_before_exec('''
if words == ['bcmr', 'serve']:
    import subprocess
    child = subprocess.Popen(['/bin/sh', '-c', ' '.join(words)], stdout=subprocess.PIPE)
    remaining = 32768
    while remaining:
        chunk = os.read(child.stdout.fileno(), min(4096, remaining))
        if not chunk:
            break
        os.write(1, chunk)
        remaining -= len(chunk)
    child.kill()
    child.wait()
    sys.exit(9)
''')
        result = self.run_bcmr("copy", "-f", "-y", src, dst)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "old destination bytes")

    # The no-clobber contract must survive creation after an existence probe.
    def test_legacy_upload_target_created_during_connection_is_preserved(self):
        source = self.put(self.local, "race-source.txt", "incoming")
        destination = self.home / "race-target.txt"
        self.inject_transport_before_exec('''
if any('cat ' in word and %r in word for word in words):
    with open(%r, 'w') as stream:
        stream.write('concurrent target')
''' % (str(destination), str(destination)))
        result = self.run_bcmr("copy", source, f"legacy:{destination}")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "concurrent target")

    # Resume may restart mismatching/oversized files and append an empty prefix.
    def test_serve_upload_resume_can_restart_or_append_zero_without_force(self):
        source = self.put(self.local, "resume-source.txt", "replacement")
        for name, content in (("mismatch", "bad prefix"),
                              ("oversized", "destination is much larger than source"),
                              ("empty", "")):
            with self.subTest(case=name):
                destination = self.put(self.home, "resume-" + name, content)
                self.assert_ok(self.run_bcmr("copy", "-C", source, f"fixture:{destination}"))
                self.assertEqual(destination.read_text(), "replacement")

    # Empty trees must not hide errors from either initial or nested mkdir.
    def test_legacy_empty_directory_upload_rejects_blocked_remote_tree(self):
        source = self.local / "emptytree"
        source.mkdir()
        root_blocker = self.put(self.home, "blocked-root", "root bytes")
        result = self.run_bcmr("copy", "-r", "-P", "2", source, f"legacy:{root_blocker}")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("blocked-root", result.stderr)
        self.assertEqual(root_blocker.read_text(), "root bytes")

    def test_legacy_empty_subdirectory_upload_rejects_blocked_child(self):
        source = self.local / "nestedtree"
        (source / "child").mkdir(parents=True)
        destination = self.home / "tree-parent"
        blocker = self.put(destination, "nestedtree/child", "child bytes")
        result = self.run_bcmr("copy", "-r", "-P", "2", source, f"legacy:{destination}")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("tree-parent", result.stderr)
        self.assertEqual(blocker.read_text(), "child bytes")

    # The direct striped path must create missing parents like ordinary GET.
    def test_direct_striped_download_creates_missing_file_parents(self):
        source = self.home / "large-source.bin"
        with source.open("wb") as stream:
            stream.write(b"striped source marker")
            stream.truncate(64 * 1024 * 1024 + 1)
        destination = self.dest / "missing/parents/target.bin"
        result = self.run_bcmr("copy", "--direct", "direct", "-P", "2",
                               f"fixture:{source}", destination)
        self.assert_ok(result)
        self.assertEqual(destination.stat().st_size, source.stat().st_size)
        def digest(path):
            hasher = hashlib.sha256()
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    hasher.update(block)
            return hasher.digest()
        self.assertEqual(digest(destination), digest(source))

    def inject_transport_before_exec(self, code):
        ssh = self.bin / "ssh"
        content = ssh.read_text()
        marker = "# OpenSSH joins command arguments and executes them with the login shell."
        self.assertIn(marker, content)
        ssh.write_text(content.replace(marker, code + "\n" + marker))


if __name__ == "__main__":
    unittest.main(verbosity=2)
