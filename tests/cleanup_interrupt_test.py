#!/usr/bin/env python3
"""SIGINT during real SFTP move-cleanup validation must retain every source.

Only the subsystem byte stream is delayed, after the destination is published.
All file operations are performed by the real OpenSSH sftp-server.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import re
import sys
import tempfile
import time
import unittest

import composite_transfer_test as composite
import remote_overwrite_test as overwrite
import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires the real POSIX cleanup shell")
class SshCleanupGateTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="bcmr-cleanup-gate-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        self.source.write_text("captured bytes")
        script_source = Path(os.environ.get(
            "BCMR_TEST_CLEANUP_SOURCE",
            str(Path(__file__).resolve().parents[1] / "src/core/remote/cleanup.rs"))).read_text()

        def script(name):
            return re.search(r'const ' + name + r': &str = r#"(.*?)"#;',
                             script_source, re.S).group(1)
        functions = script("STAT_FUNCTIONS")
        capture = subprocess.run(["sh", "-c", functions + script("SNAPSHOT"),
                                  "capture", str(self.source)], capture_output=True, check=True)
        kind, metadata, parent_identity, _ = capture.stdout.split(b"\0")[:4]
        self.command = ["sh", "-c", functions + script("REMOVE_LEAF"), "cleanup",
                        str(self.root), self.source.name, parent_identity.decode(), kind.decode(),
                        metadata.decode(), metadata.decode().rsplit("|", 1)[0]]

    def assert_restored(self, decision):
        result = subprocess.run(self.command, input=decision, capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.source.read_text(), "captured bytes")
        self.assertEqual(list(self.root.glob(".bcmr.move.*")), [])

    def test_eof_without_a_commit_preserves_the_quarantined_source(self):
        self.assert_restored(b"")

    def test_explicit_abort_restores_the_quarantined_source(self):
        self.assert_restored(b"abort\n")

    def test_explicit_commit_removes_the_captured_source(self):
        result = subprocess.run(self.command, input=b"commit\n", capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.source.exists())
        self.assertEqual(list(self.root.glob(".bcmr.move.*")), [])

    def test_source_change_while_awaiting_commit_is_preserved(self):
        child = subprocess.Popen(self.command, stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            fields = []
            for _ in range(3):
                field = bytearray()
                while True:
                    byte = child.stdout.read(1)
                    self.assertTrue(byte, "cleanup exited before requesting a commit")
                    if byte == b"\0":
                        break
                    field.extend(byte)
                    self.assertLess(len(field), 256)
                fields.append(bytes(field))
            self.assertEqual(fields[0], b"quarantine")
            self.assertEqual(fields[2], b"ready")
            quarantined = self.root / fields[1].decode() / self.source.name
            quarantined.write_text("new uncopied bytes")
            stdout, stderr = child.communicate(b"commit\n", timeout=5)
            self.assertNotEqual(child.returncode, 0, stdout + stderr)
            self.assertEqual(self.source.read_text(), "new uncopied bytes")
            self.assertEqual(list(self.root.glob(".bcmr.move.*")), [])
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)


@unittest.skipUnless(os.name == "posix", "requires real POSIX SSH cleanup")
class SshMoveDeliveryTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    inject_transport_before_exec = overwrite.RemoteOverwriteTests.inject_transport_before_exec

    def test_remote_resume_of_a_local_alias_cannot_delete_the_only_entry(self):
        source = self.put(self.home, "shared-source", "only shared payload")
        result = self.run_bcmr("move", "-C", "fixture:" + str(source), source)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(source.is_file(), "move removed the source and its local alias")
        self.assertEqual(source.read_text(), "only shared payload")

    def test_local_resume_of_an_ssh_alias_cannot_delete_the_only_entry(self):
        source = self.put(self.local, "shared-source", "only local payload")
        result = self.run_bcmr("move", "-C", source, "fixture:" + str(source))
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(source.is_file(), "move removed the source and its SSH alias")
        self.assertEqual(source.read_text(), "only local payload")

    def change_destination_before_source_quarantine(self, destination, link=False):
        marker = self.root / "destination-changed"
        mutation = ("os.unlink(%r); os.symlink('changed link target', %r)" %
                    (str(destination), str(destination))) if link else (
                    "open(%r, 'w').write('changed delivered bytes')" % str(destination))
        self.inject_transport_before_exec('''
if 'bcmr-cleanup-leaf' in ' '.join(words) and os.path.lexists(%r):
    %s
    open(%r, 'w').write('destination changed after publication')
''' % (str(destination), mutation, str(marker)))
        return marker

    def test_changed_delivered_file_preserves_the_remote_source(self):
        source = self.put(self.home, "source-file", "captured remote file")
        destination = self.dest / "delivered-file"
        marker = self.change_destination_before_source_quarantine(destination)
        result = self.run_bcmr("move", "fixture:" + str(source), destination)
        self.assertTrue(marker.exists(), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(source.read_text(), "captured remote file")
        self.assertEqual(destination.read_text(), "changed delivered bytes")

    def test_changed_delivered_link_preserves_the_remote_source(self):
        source = self.home / "source-link"
        source.symlink_to("captured link target")
        original = source.lstat()
        destination = self.dest / "delivered-link"
        marker = self.change_destination_before_source_quarantine(destination, link=True)
        result = self.run_bcmr("move", "fixture:" + str(source), destination)
        self.assertTrue(marker.exists(), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        # Portable SSH cleanup may retain a symlink in the private quarantine
        # rather than risk replacing an entry while restoring its name.
        # Python 3.9's precise glob leaf calls exists(), hiding dangling links.
        recovery = [directory / source.name for directory in self.home.glob(".bcmr.move.*")
                    if directory.is_dir()]
        preserved = [path for path in [source, *recovery]
                     if path.is_symlink()]
        self.assertEqual(len(preserved), 1, result.stdout + result.stderr)
        retained = preserved[0]
        actual = retained.lstat()
        self.assertEqual((actual.st_dev, actual.st_ino), (original.st_dev, original.st_ino))
        self.assertEqual(os.readlink(retained), "captured link target")
        if retained != source:
            self.assertIn(str(retained.parent), result.stderr)
            self.assertIn("recover", result.stderr)
            self.assertEqual(list(retained.parent.iterdir()), [retained])
        self.assertEqual(os.readlink(destination), "changed link target")

    def test_preserved_file_times_survive_delivery_hash_and_cleanup_validation(self):
        source = self.put(self.home, "dated-file", "preserved remote payload")
        os.utime(source, (1_500_000_001, 1_500_000_002))
        destination = self.dest / "dated-file"
        self.assert_ok(self.run_bcmr("move", "-p", "fixture:" + str(source), destination))
        metadata = destination.stat()
        self.assertEqual(int(metadata.st_atime), 1_500_000_001)
        self.assertEqual(int(metadata.st_mtime), 1_500_000_002)
        self.assertEqual(destination.read_text(), "preserved remote payload")
        self.assertFalse(source.exists())

    def test_uploaded_file_times_survive_delivery_capture_and_cleanup_validation(self):
        source = self.put(self.local, "dated-file", "preserved local payload")
        os.utime(source, (1_500_000_003, 1_500_000_004))
        destination = self.home / "dated-file"
        self.assert_ok(self.run_bcmr("move", "-p", source, "fixture:" + str(destination)))
        metadata = destination.stat()
        self.assertEqual(int(metadata.st_atime), 1_500_000_003)
        self.assertEqual(int(metadata.st_mtime), 1_500_000_004)
        self.assertEqual(destination.read_text(), "preserved local payload")
        self.assertFalse(source.exists())


class _TimestampPermissionCases:
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    backend_options = ()

    def deny_timestamp_changes(self, destination):
        subprocess.run(["/bin/chmod", "+a", "everyone deny writeattr", str(destination)],
                       check=True, capture_output=True)
        metadata = destination.stat()
        # Exercise the real filesystem permission, not a mocked chmod result.
        with self.assertRaises(PermissionError):
            os.utime(destination, ns=(metadata.st_atime_ns, metadata.st_mtime_ns))
        self.assertEqual(destination.read_text(), "matching delivered bytes")
        return metadata.st_ino

    def test_resumed_upload_needs_no_destination_timestamp_permission_without_preserve(self):
        source = self.put(self.local, "source", "matching delivered bytes")
        destination = self.put(self.home, "destination", "matching delivered bytes")
        inode = self.deny_timestamp_changes(destination)
        result = self.run_bcmr("move", "-C", *self.backend_options,
                               source, "fixture:" + str(destination))
        self.assert_ok(result)
        self.assertFalse(source.exists())
        self.assertEqual(destination.stat().st_ino, inode, "resume replaced the destination")
        self.assertEqual(destination.read_text(), "matching delivered bytes")

    def test_resumed_download_needs_no_destination_timestamp_permission_without_preserve(self):
        source = self.put(self.home, "source", "matching delivered bytes")
        destination = self.put(self.dest, "destination", "matching delivered bytes")
        inode = self.deny_timestamp_changes(destination)
        result = self.run_bcmr("move", "-C", *self.backend_options,
                               "fixture:" + str(source), destination)
        self.assert_ok(result)
        self.assertFalse(source.exists())
        self.assertEqual(destination.stat().st_ino, inode, "resume replaced the destination")
        self.assertEqual(destination.read_text(), "matching delivered bytes")


@unittest.skipUnless(sys.platform == "darwin", "requires Darwin deny-writeattr ACL")
class SshTimestampPermissionTests(_TimestampPermissionCases, unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp


@unittest.skipUnless(sys.platform == "darwin" and composite.SFTP_SERVER,
                     "requires Darwin ACL and real OpenSSH sftp-server")
class SftpTimestampPermissionTests(_TimestampPermissionCases, unittest.TestCase):
    setUp = composite.SftpOnlyTests.setUp
    backend_options = ("--remote-backend=sftp",)



@unittest.skipUnless(os.name == "posix" and composite.SFTP_SERVER,
                     "requires POSIX and a real OpenSSH sftp-server")
class CleanupInterruptTests(unittest.TestCase):
    setUp = composite.SftpOnlyTests.setUp
    put = fixture.SourceStarTests.put

    def delay_cleanup_read(self, published):
        self.marker = self.root / "cleanup-read-started"
        self.env.update(CLEANUP_INTERRUPT_PUBLISHED=str(published),
                        CLEANUP_INTERRUPT_MARKER=str(self.marker))
        proxy = self.bin / "sftp-delay-proxy"
        proxy.write_text('''#!/usr/bin/env python3
import json, os, struct, subprocess, sys, threading, time
from pathlib import Path
server = subprocess.Popen([os.environ['SFTP_TEST_SERVER']], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE)
def output():
    while True:
        block = os.read(server.stdout.fileno(), 65536)
        if not block: return
        os.write(1, block)
thread = threading.Thread(target=output, daemon=True)
thread.start()
def exact(count):
    result = bytearray()
    while len(result) < count:
        block = os.read(0, count - len(result))
        if not block: return None
        result.extend(block)
    return bytes(result)
delayed = False
try:
    while True:
        header = exact(4)
        if header is None: break
        payload = exact(struct.unpack('>I', header)[0])
        if payload is None: break
        published = Path(os.environ['CLEANUP_INTERRUPT_PUBLISHED'])
        if payload[0] == 5 and published.exists() and not delayed:
            delayed = True
            Path(os.environ['CLEANUP_INTERRUPT_MARKER']).write_text(json.dumps(
                dict(request_type=5, phase='read-after-publication', size=published.stat().st_size)))
            time.sleep(4)
        server.stdin.write(header + payload)
        server.stdin.flush()
finally:
    server.stdin.close()
    thread.join(timeout=2)
    try: server.wait(timeout=2)
    except subprocess.TimeoutExpired:
        server.kill(); server.wait()
''')
        proxy.chmod(0o755)
        transport = self.bin / "ssh"
        script = transport.read_text()
        script = script.replace(
            "os.execv(os.environ['SFTP_TEST_SERVER'], ['sftp-server'])",
            "os.execv(" + repr(str(proxy)) + ", ['sftp-delay-proxy'])")
        transport.write_text(script)

    def run_interrupted_cleanup(self, mixed=False, json_mode=False):
        remote = self.put(self.home, "remote-source", "captured remote payload")
        published = self.dest / remote.name
        self.delay_cleanup_read(published)
        sources = ["fixture:" + str(remote)]
        local = None
        if mixed:
            local = self.put(self.local, "local-source", "captured local payload")
            sources.append(str(local))
        command = [fixture.BCMR] + (["--json"] if json_mode else [])
        command += ["move", "--remote-backend=sftp", "--progress=off", *sources, str(self.dest)]
        child = subprocess.Popen(command, env=self.env, cwd=self.local,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 15
            while not self.marker.exists():
                if child.poll() is not None:
                    self.fail("move exited before cleanup hash: " + str(child.communicate()))
                if time.monotonic() >= deadline:
                    self.fail("did not observe the post-publication SFTP cleanup READ")
                time.sleep(0.01)
            phase = json.loads(self.marker.read_text())
            self.assertEqual(phase["phase"], "read-after-publication")
            self.assertEqual(published.read_text(), "captured remote payload")
            if local is not None:
                self.assertEqual((self.dest / local.name).read_text(), "captured local payload")
            child.send_signal(signal.SIGINT)
            stdout, stderr = child.communicate(timeout=8)
            self.assertEqual(child.returncode, 130, stdout + stderr)
            self.assertEqual(remote.read_text(), "captured remote payload")
            if local is not None:
                self.assertEqual(local.read_text(), "captured local payload")
            self.assertEqual(list(self.home.glob(".bcmr.move.*")), [])
            if json_mode:
                events = [json.loads(line) for line in stdout.splitlines() if line.strip()]
                terminals = [event for event in events if event.get("type") == "result"]
                self.assertEqual(len(terminals), 1, events)
                self.assertEqual(terminals[0].get("status"), "error", events)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)

    def test_sigint_during_cleanup_hash_preserves_remote_source(self):
        self.run_interrupted_cleanup()

    def test_sigint_before_cleanup_mutations_preserves_mixed_sources(self):
        self.run_interrupted_cleanup(mixed=True)

    def test_json_cleanup_interrupt_retains_typed_exit_status(self):
        self.run_interrupted_cleanup(json_mode=True)


if __name__ == "__main__":
    unittest.main()
