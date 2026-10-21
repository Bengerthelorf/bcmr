#!/usr/bin/env python3
"""Real CLI overwrite contracts with only SSH transport replaced.

BCMR=/absolute/path/to/bcmr python3 tests/remote_overwrite_test.py
"""
import os
import hashlib
import shlex
import shutil
import subprocess
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

    # Metadata/verification staging must retain ordinary parent creation rules.
    def test_single_file_preserve_or_verify_creates_missing_parents(self):
        for mode in self.MODES:
            for flag in ("-p", "-V"):
                with self.subTest(mode=mode, flag=flag):
                    source = self.put(self.home if mode.startswith("download") else self.local,
                                      f"{mode}-{flag[1:]}-source.txt", "nested publication bytes")
                    base = self.home if mode.startswith("upload") else self.dest
                    destination = base / f"{mode}-{flag[1:]}-missing" / "nested" / "payload.txt"
                    host = "legacy" if mode.endswith("legacy") else "fixture"
                    src = f"{host}:{source}" if mode.startswith("download") else source
                    dst = f"{host}:{destination}" if mode.startswith("upload") else destination
                    self.assert_ok(self.run_bcmr("copy", flag, src, dst))
                    self.assertEqual(destination.read_text(), "nested publication bytes")
                    self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

    # Explicit -p takes precedence over the previous target's ordinary mode.
    def test_forced_preserve_uses_source_mode_on_all_routes(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                source, destination, src, dst = self.route(mode)
                source.chmod(0o751)
                destination.chmod(0o600)
                self.assert_ok(self.run_bcmr("copy", "-f", "-y", "-p", src, dst))
                self.assertEqual(destination.stat().st_mode & 0o777, 0o751)
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
if any('cat ' in word and '>' in word for word in words):
    with open(%r, 'w') as stream:
        stream.write('concurrent target')
''' % str(destination))
        result = self.run_bcmr("copy", source, f"legacy:{destination}")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "concurrent target")

    def install_failed_upload_cat(self):
        fault_bin = self.root / "fault-bin"
        fault_bin.mkdir()
        cat = fault_bin / "cat"
        cat.write_text("#!/bin/sh\n/bin/dd bs=1 count=2 2>/dev/null\nexit 9\n")
        cat.chmod(0o755)
        self.inject_transport_before_exec('''
if any('cat ' in word and '>' in word for word in words):
    os.environ['PATH'] = %r + os.pathsep + os.environ['PATH']
''' % str(fault_bin))

    # Failed PUT must never truncate an existing final file before committing.
    def test_failed_forced_legacy_upload_preserves_existing_target(self):
        _, destination, src, dst = self.route("upload-legacy")
        self.install_failed_upload_cat()
        result = self.run_bcmr("copy", "-f", "-y", src, dst)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "old destination bytes")
        self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

    # An interrupted first PUT may leave a private stage, never a partial final.
    def test_failed_new_legacy_upload_leaves_no_visible_target(self):
        source = self.put(self.local, "new-source.txt", "complete upload payload")
        destination = self.home / "new-target.txt"
        self.install_failed_upload_cat()
        result = self.run_bcmr("copy", source, f"legacy:{destination}")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(destination.exists(), "failed PUT exposed a partial final file")
        self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

    # Source size is captured before the SSH size probe; EOF must match it.
    def test_short_local_read_refuses_legacy_upload_publication(self):
        source, destination, src, dst = self.route("upload-legacy")
        marker = self.root / "truncated-source"
        self.inject_transport_before_exec('''
if any("stat -c '%%s'" in word for word in words) and not os.path.exists(%r):
    os.truncate(%r, 2)
    open(%r, 'w').close()
''' % (str(marker), str(source), str(marker)))
        result = self.run_bcmr("copy", "-f", "-y", src, dst)
        self.assertTrue(marker.exists(), "source truncation did not reach the size probe")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "old destination bytes")
        self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

    # Verification belongs to the unpublished payload, including forced PUT.
    def test_failed_legacy_upload_verification_preserves_existing_target(self):
        _, destination, src, dst = self.route("upload-legacy")
        self.inject_transport_before_exec('''
import shlex
if len(words) == 1 and '>' not in words[0] and shlex.split(words[0])[:1] == ['cat']:
    os.execv('/bin/sh', ['sh', '-c', "printf 'incorrect verification payload'"])
''')
        result = self.run_bcmr("copy", "-f", "-y", "-V", src, dst)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "old destination bytes")
        self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

    # Attribute failure must not publish the already received forced payload.
    def test_failed_legacy_upload_attributes_preserve_existing_target(self):
        _, destination, src, dst = self.route("upload-legacy")
        self.inject_transport_before_exec('''
if any('touch -m' in word for word in words):
    sys.stderr.write('fixture attribute failure\\n')
    sys.exit(9)
''')
        result = self.run_bcmr("copy", "-f", "-y", "-p", src, dst)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "old destination bytes")
        self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

    def install_publish_race(self, destination, kind):
        marker = self.root / "publication-race"
        outside = self.put(self.root, "outside-race-target", "outside bytes")
        self.inject_transport_before_exec('''
if any('bcmr-upload-commit' in word for word in words):
    destination = %r
    if os.path.lexists(destination):
        os.unlink(destination)
    if %r == 'directory':
        os.mkdir(destination)
    elif %r == 'symlink':
        os.symlink(%r, destination)
    else:
        with open(destination, 'w') as stream:
            stream.write('concurrent target')
    open(%r, 'w').close()
''' % (str(destination), kind, kind, str(outside), str(marker)))
        return marker, outside

    # A late directory must not turn the file's final path into a container.
    def test_forced_legacy_upload_directory_race_preserves_competing_directory(self):
        _, destination, src, dst = self.route("upload-legacy")
        marker, _ = self.install_publish_race(destination, "directory")
        result = self.run_bcmr("copy", "-f", "-y", src, dst)
        self.assertTrue(marker.exists(), "separate publication phase was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(destination.is_dir())
        self.assertEqual(list(destination.iterdir()), [])

    def test_forced_legacy_upload_symlink_race_never_writes_link_target(self):
        _, destination, src, dst = self.route("upload-legacy")
        marker, outside = self.install_publish_race(destination, "symlink")
        self.assert_ok(self.run_bcmr("copy", "-f", "-y", src, dst))
        self.assertTrue(marker.exists(), "separate publication phase was not reached")
        self.assertFalse(destination.is_symlink())
        self.assertEqual(destination.read_text(), "new replacement bytes")
        self.assertEqual(outside.read_text(), "outside bytes")

    def test_legacy_upload_target_created_at_publication_is_preserved(self):
        source = self.put(self.local, "commit-race-source.txt", "incoming")
        destination = self.home / "commit-race-target.txt"
        marker, _ = self.install_publish_race(destination, "file")
        result = self.run_bcmr("copy", source, f"legacy:{destination}")
        self.assertTrue(marker.exists(), "separate publication phase was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "concurrent target")

    # Staging must retain existing ACL/xattr security, as truncation did.
    def test_forced_upload_preserves_existing_extended_attributes(self):
        xattr = shutil.which("xattr")
        if not hasattr(os, "setxattr") and not xattr:
            self.skipTest("extended attributes are unavailable")
        for mode, flags in (("upload-legacy", ()), ("upload-serve", ("-p",)),
                            ("upload-serve", ("-V",))):
            with self.subTest(mode=mode, flags=flags):
                _, destination, src, dst = self.route(mode)
                try:
                    if hasattr(os, "setxattr"):
                        os.setxattr(destination, "user.bcmr-preserve", b"existing label")
                    else:
                        subprocess.run([xattr, "-w", "user.bcmr-preserve", "existing label", str(destination)],
                                       check=True, capture_output=True)
                except OSError as error:
                    self.skipTest(f"fixture filesystem cannot store xattrs: {error}")
                self.assert_ok(self.run_bcmr("copy", "-f", "-y", *flags, src, dst))
                self.assertEqual(destination.read_text(), "new replacement bytes")
                try:
                    if hasattr(os, "getxattr"):
                        actual = os.getxattr(destination, "user.bcmr-preserve")
                    else:
                        result = subprocess.run([xattr, "-px", "user.bcmr-preserve", str(destination)],
                                                check=True, capture_output=True)
                        actual = bytes.fromhex(result.stdout.decode())
                except (OSError, subprocess.CalledProcessError):
                    actual = None
                self.assertEqual(actual, b"existing label", "staged overwrite dropped an existing xattr")

    # A failed metadata clone must fail closed before changing the old target.
    def test_failed_upload_security_copy_preserves_existing_target(self):
        _, destination, src, dst = self.route("upload-legacy")
        marker = self.root / "failed-security-copy"
        self.inject_transport_before_exec('''
if any('bcmr-upload-prepare' in word for word in words):
    words = [word.replace('/bin/cp -p', '/usr/bin/false')
                 .replace('cp --attributes-only', 'false --attributes-only') for word in words]
    open(%r, 'w').close()
''' % str(marker))
        result = self.run_bcmr("copy", "-f", "-y", src, dst)
        self.assertTrue(marker.exists(), "security preparation phase was not reached")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(destination.read_text(), "old destination bytes")
        self.assertEqual(list(destination.parent.glob(".bcmr.upload.*")), [])

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
