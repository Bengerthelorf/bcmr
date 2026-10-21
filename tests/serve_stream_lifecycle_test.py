#!/usr/bin/env python3
"""Real native serve framing: Done boundaries, connection reuse and peer EOF.

BCMR=/absolute/path/to/bcmr python3 tests/serve_stream_lifecycle_test.py
"""
import os
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest


BCMR = str(Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve())


def string(value):
    encoded = str(value).encode("utf-8")
    return struct.pack("<I", len(encoded)) + encoded


def frame(payload):
    return struct.pack("<I", len(payload)) + payload


def data(payload):
    return b"\x84" + struct.pack("<I", len(payload)) + payload


class ServeStreamLifecycleTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="bcmr-serve-lifecycle-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.target = self.root / "payload"

    def exchange(self, *requests):
        # Fixed v2 HELLO with no optional compression/AEAD capabilities.
        wire = frame(b"\x01\x02\x00") + b"".join(map(frame, requests))
        result = subprocess.run([BCMR, "serve", "--root", str(self.root)],
                                input=wire, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        replies = []
        output = result.stdout
        while output:
            self.assertGreaterEqual(len(output), 4, "truncated response header")
            length = struct.unpack("<I", output[:4])[0]
            self.assertGreater(length, 0)
            self.assertLessEqual(length, len(output) - 4, "truncated response payload")
            replies.append(output[4:4 + length])
            output = output[4 + length:]
        self.assertTrue(replies)
        self.assertEqual(replies[0], b"\x81\x02\x00")
        return replies[1:]

    def put(self, length, overwrite=False):
        return b"\x06" + string(self.target) + struct.pack("<QQB", length, 0, overwrite)

    def put_chunked(self, length):
        return b"\x0d" + string(self.target) + struct.pack("<QQ", 0, length)

    def stat(self):
        return b"\x03" + string(self.target)

    def assert_error(self, reply):
        self.assertEqual(reply[0], 0x83, f"expected an error, got {reply!r}")

    # Control proves these fixtures negotiate, publish and reuse real framing.
    def test_regular_put_with_done_can_reuse_the_connection(self):
        replies = self.exchange(self.put(3), data(b"abc"), b"\x09", self.stat())
        self.assertEqual([reply[0] for reply in replies], [0x82, 0x85])
        self.assertEqual(struct.unpack("<Q", replies[1][1:9])[0], 3)
        self.assertEqual(self.target.read_bytes(), b"abc")

    # Catches replying at the byte count while leaving Done in the session.
    def test_chunked_put_consumes_done_before_the_next_request(self):
        replies = self.exchange(self.put_chunked(3), data(b"abc"), b"\x09", self.stat())
        self.assertEqual([reply[0] for reply in replies], [0x82, 0x85])
        self.assertEqual(self.target.read_bytes(), b"abc")

    def test_empty_chunked_put_also_consumes_done(self):
        replies = self.exchange(self.put_chunked(0), b"\x09", self.stat())
        self.assertEqual([reply[0] for reply in replies], [0x82, 0x85])
        self.assertEqual(self.target.read_bytes(), b"")

    # Exact byte count is not a complete request when the peer cancels at EOF.
    def test_chunked_put_rejects_eof_without_done_after_full_payload(self):
        replies = self.exchange(self.put_chunked(3), data(b"abc"))
        self.assertEqual(len(replies), 1)
        self.assert_error(replies[0])

    def test_regular_put_rejects_eof_without_done_before_publication(self):
        for overwrite in (False, True):
            with self.subTest(overwrite=overwrite):
                if overwrite:
                    self.target.write_bytes(b"old")
                replies = self.exchange(self.put(3, overwrite), data(b"new"))
                self.assertEqual(len(replies), 1)
                self.assert_error(replies[0])
                if overwrite:
                    self.assertEqual(self.target.read_bytes(), b"old")
                else:
                    self.assertFalse(self.target.exists())
                self.assertEqual(list(self.root.glob(".bcmr*")), [])

    # A second data block after the declared boundary must not receive success.
    def test_chunked_put_rejects_data_after_the_exact_boundary(self):
        replies = self.exchange(self.put_chunked(3), data(b"abc"), data(b"x"), b"\x09")
        self.assert_error(replies[0])
        self.assertEqual(self.target.read_bytes(), b"abc")

    def test_chunked_put_rejects_done_before_the_declared_boundary(self):
        replies = self.exchange(self.put_chunked(4), data(b"abc"), b"\x09")
        self.assertEqual(len(replies), 1)
        self.assert_error(replies[0])


if __name__ == "__main__":
    unittest.main()
