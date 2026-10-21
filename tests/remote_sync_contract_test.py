#!/usr/bin/env python3
"""Requested durability/transport contracts across real CLI and filesystem paths.

Only SSH is replaced. The capability fixture forwards a real serve session,
removing CAP_SYNC from both handshake directions to model an older peer.
"""
import os
import unittest

import remote_overwrite_test as overwrite
import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem and shell")
class RemoteSyncContractTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    inject_transport_before_exec = overwrite.RemoteOverwriteTests.inject_transport_before_exec

    def assert_refused_before_writes(self, result, destination):
        self.assertFalse(destination.exists() or destination.is_symlink(), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(result.stderr.lower(), r"sync|durab|direct|transport")

    def test_legacy_file_sync_refuses_before_missing_parent_creation(self):
        source = self.put(self.local, "source.txt", "durable bytes requested")
        for parallel in (1, 2):
            with self.subTest(parallel=parallel):
                destination = self.home / f"legacy-file-{parallel}"
                result = self.run_bcmr("copy", "--sync", "-P", parallel, source,
                                       f"legacy:{destination}/payload.txt")
                self.assert_refused_before_writes(result, destination)
                self.assertEqual(source.read_text(), "durable bytes requested")

    def test_legacy_empty_directory_sync_refuses_before_mkdir(self):
        source = self.local / "empty-tree"
        source.mkdir()
        for parallel in (1, 2):
            with self.subTest(parallel=parallel):
                destination = self.home / f"legacy-empty-{parallel}"
                result = self.run_bcmr("copy", "-r", "--sync", "-P", parallel, source,
                                       f"legacy:{destination}/")
                self.assert_refused_before_writes(result, destination)

    def test_link_preserving_upload_sync_refuses_before_mkdir_or_move_cleanup(self):
        for operation in ("copy", "move"):
            with self.subTest(operation=operation):
                source = self.local / operation
                payload = self.put(source, "payload.txt", "must remain at source")
                link = source / "link"
                link.symlink_to("payload.txt")
                destination = self.home / (operation + "-sync-links")
                options = ["--no-deref"] if operation == "copy" else []
                result = self.run_bcmr(operation, "-r", "--sync", *options, source,
                                       f"fixture:{destination}/")
                self.assert_refused_before_writes(result, destination)
                self.assertEqual(payload.read_text(), "must remain at source")
                self.assertEqual(os.readlink(link), "payload.txt")

    def test_staged_serve_sync_refuses_before_any_directory_or_stage_creation(self):
        for flag in ("-p", "-V"):
            for directory in (False, True):
                with self.subTest(flag=flag, directory=directory):
                    label = flag[1:] + ("-tree" if directory else "-file")
                    source = self.put(self.local, label + "/payload.txt", "sync validation bytes")
                    if directory:
                        source = source.parent
                    destination = self.home / ("serve-staged-" + label)
                    options = ["-r"] if directory else []
                    target = f"fixture:{destination}" + ("/" if directory else "")
                    result = self.run_bcmr("copy", "--sync", flag, *options, source,
                                           target)
                    self.assert_refused_before_writes(result, destination)
                    self.assertFalse(any(self.home.rglob(".bcmr-upload-*")))

    def remove_peer_sync_capability(self):
        marker = self.root / "sync-capability-removed"
        self.inject_transport_before_exec('''
if words == ['bcmr', 'serve']:
    import struct
    import subprocess
    import threading
    def exact(fd, count):
        result = bytearray()
        while len(result) < count:
            block = os.read(fd, count - len(result))
            if not block:
                raise RuntimeError('capability fixture saw a short frame')
            result.extend(block)
        return bytes(result)
    def frame(fd):
        header = exact(fd, 4)
        return bytearray(exact(fd, struct.unpack('<I', header)[0]))
    hello = frame(0)
    if len(hello) != 3 or hello[0] != 1:
        sys.exit('capability fixture expected HELLO')
    hello[2] &= ~0x10
    child = subprocess.Popen(words, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    child.stdin.write(struct.pack('<I', len(hello)) + hello)
    child.stdin.flush()
    welcome = frame(child.stdout.fileno())
    if len(welcome) != 3 or welcome[0] != 0x81:
        child.kill()
        sys.exit('capability fixture expected WELCOME')
    welcome[2] &= ~0x10
    with open(%r, 'w') as stream:
        stream.write('real peer negotiated without CAP_SYNC')
    sys.stdout.buffer.write(struct.pack('<I', len(welcome)) + welcome)
    sys.stdout.buffer.flush()
    def forward_input():
        try:
            while True:
                block = os.read(0, 65536)
                if not block:
                    break
                child.stdin.write(block)
                child.stdin.flush()
        except BrokenPipeError:
            pass
        finally:
            child.stdin.close()
    threading.Thread(target=forward_input, daemon=True).start()
    while True:
        block = os.read(child.stdout.fileno(), 65536)
        if not block:
            break
        sys.stdout.buffer.write(block)
        sys.stdout.buffer.flush()
    sys.exit(child.wait())
''' % str(marker))
        return marker

    def test_serve_sync_requires_negotiated_peer_capability_before_writes(self):
        marker = self.remove_peer_sync_capability()
        for directory in (False, True):
            with self.subTest(directory=directory):
                label = "tree" if directory else "file"
                source = self.put(self.local, label + "/payload.txt", "peer capability bytes")
                if directory:
                    source = source.parent
                destination = self.home / ("peer-no-sync-" + label)
                options = ["-r", "-P", "2"] if directory else []
                result = self.run_bcmr("copy", "--sync", *options, source,
                                       f"fixture:{destination}/")
                self.assertTrue(marker.exists(), "real peer capability fixture was not reached")
                self.assert_refused_before_writes(result, destination)

    def test_explicit_direct_refuses_link_preserving_ssh_route_before_writes(self):
        for operation, download in (("copy", False), ("copy", True), ("move", False)):
            with self.subTest(operation=operation, download=download):
                label = operation + ("-download" if download else "-upload")
                source_base = self.home if download else self.local
                source = source_base / (label + "-link")
                source.symlink_to("missing target")
                destination = (self.dest if download else self.home) / (label + "-direct")
                source_arg = f"fixture:{source}" if download else source
                destination_arg = destination if download else f"fixture:{destination}/"
                options = ["--no-deref"] if operation == "copy" else []
                result = self.run_bcmr(operation, "--direct", "direct", "-f", "-y", *options,
                                       source_arg, destination_arg)
                self.assert_refused_before_writes(result, destination)
                self.assertEqual(os.readlink(source), "missing target")

    def test_unsupported_sync_and_transport_dry_runs_can_preview_without_writes(self):
        source = self.put(self.local, "preview/payload.txt", "preview bytes")
        cases = (("legacy", ["--sync"]),
                 ("fixture", ["--sync", "-p"]),
                 ("fixture", ["--sync", "--no-deref"]),
                 ("fixture", ["--direct", "direct", "--no-deref"]))
        for index, (host, flags) in enumerate(cases):
            with self.subTest(host=host, flags=flags):
                destination = self.home / ("preview-" + str(index))
                result = self.run_bcmr("copy", "--dry-run", *flags, source,
                                       f"{host}:{destination}/")
                self.assert_ok(result)
                self.assertFalse(destination.exists())

    def test_ordinary_serve_upload_and_download_sync_remain_supported(self):
        source = self.put(self.local, "source.txt", "supported durable bytes")
        destination = self.home / "durable/payload.txt"
        self.assert_ok(self.run_bcmr("copy", "--sync", source, f"fixture:{destination}"))
        self.assertEqual(destination.read_text(), "supported durable bytes")
        landed = self.dest / "download/payload.txt"
        self.assert_ok(self.run_bcmr("copy", "--sync", f"fixture:{destination}", landed))
        self.assertEqual(landed.read_text(), "supported durable bytes")
        empty = self.local / "empty-tree/nested/empty"
        empty.mkdir(parents=True)
        remote_empty = self.home / "durable-empty"
        self.assert_ok(self.run_bcmr("copy", "-r", "--sync", empty.parents[1],
                                     f"fixture:{remote_empty}/"))
        self.assertTrue((remote_empty / "empty-tree/nested/empty").is_dir())

    def test_unix_local_and_downloaded_symlink_sync_preserve_target_text(self):
        for download in (False, True):
            with self.subTest(download=download):
                source = (self.home if download else self.local) / ("download-link" if download else "local-link")
                source.symlink_to("missing target with spaces")
                source_arg = f"fixture:{source}" if download else source
                destination = self.dest / ("download" if download else "local") / "nested/link"
                self.assert_ok(self.run_bcmr("copy", "--sync", "--no-deref", source_arg, destination))
                self.assertEqual(os.readlink(destination), "missing target with spaces")


if __name__ == "__main__":
    unittest.main(verbosity=2)
