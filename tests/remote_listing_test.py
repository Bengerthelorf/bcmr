#!/usr/bin/env python3
"""Legacy recursive-listing regressions; only SSH and tool failures are fixtures.

Run with BCMR=/absolute/path/to/bcmr python3 tests/remote_listing_test.py.
"""
import os
import errno
from pathlib import Path
import unittest

import star_copy_test as fixture
import remote_overwrite_test as overwrite


@unittest.skipUnless(os.name == "posix", "requires a POSIX shell and filesystem")
class RemoteListingTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    assert_files = fixture.SourceStarTests.assert_files
    inject_transport_before_exec = overwrite.RemoteOverwriteTests.inject_transport_before_exec

    def force_portable_find(self):
        # Exercise the production fallback on GNU and BSD hosts alike; the
        # actual traversal and stat operations still use the system tools.
        command = self.bin / "find"
        command.write_text("""#!/bin/sh
for arg do
    if [ "$arg" = -printf ]; then exit 1; fi
done
exec /usr/bin/find "$@"
""")
        command.chmod(0o755)

    # Excluded names need not be representable by the SSH path protocol.
    def test_excluded_non_utf8_leaf_is_pruned_before_name_validation(self):
        for portable in (False, True):
            if portable:
                self.force_portable_find()
            for host in (None, "fixture", "legacy"):
                with self.subTest(portable=portable, host=host):
                    label = f"{'portable' if portable else 'native'}-{host or 'local'}"
                    tree = (self.home if host else self.local) / label
                    self.put(tree, "keep.txt", "selected bytes")
                    try:
                        descriptor = os.open(os.fsencode(tree) + b"/bad\xff.tmp",
                                             os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    except OSError as error:
                        if error.errno in (errno.EILSEQ, errno.EINVAL):
                            self.skipTest("fixture filesystem requires UTF-8 names")
                        raise
                    os.write(descriptor, b"excluded bytes")
                    os.close(descriptor)
                    source = f"{host}:{tree}" if host else tree
                    destination = self.dest / label
                    self.assert_ok(self.run_bcmr("copy", "-r", "--exclude", r"\.tmp$",
                                                source, destination))
                    self.assert_files(destination, {"keep.txt": "selected bytes"})

    # Model a Unix peer's raw child record even on UTF-8-only client filesystems.
    def test_excluded_non_utf8_wire_entry_does_not_reject_remote_tree(self):
        self.inject_transport_before_exec(r'''
import shlex
import subprocess
command = shlex.split(words[0]) if len(words) == 1 else []
if len(command) >= 4 and command[:2] == ['sh', '-c'] and command[3] == 'bcmr-tree':
    output = subprocess.run(['/bin/sh', '-c', ' '.join(words)], capture_output=True)
    sys.stderr.buffer.write(output.stderr)
    sys.stdout.buffer.write(output.stdout)
    if output.returncode == 0:
        sys.stdout.buffer.write(b'bad\xff.tmp\0f\0' + b'1\0')
    sys.exit(output.returncode)
''')
        for host in ("fixture", "legacy"):
            with self.subTest(host=host):
                tree = self.home / ("wire-" + host)
                self.put(tree, "keep.txt", "real selected bytes")
                destination = self.dest / host
                self.assert_ok(self.run_bcmr("copy", "-r", "--exclude", r"\.tmp$",
                                            f"{host}:{tree}", destination))
                self.assert_files(destination, {"keep.txt": "real selected bytes"})

    # Broken BSD framing used to return success while copying zero files.
    def test_portable_listing_copies_relative_nested_and_unusual_names(self):
        self.force_portable_find()
        tree = self.home / "tree 'quoted\nroot"
        contents = {
            "plain.txt": "plain bytes",
            "nested dir/leaf 'quoted\nline.txt": "nested bytes",
            "$(touch INJECTED).txt": "literal shell bytes",
            "-dash.txt": "dash bytes",
        }
        for name, data in contents.items():
            self.put(tree, name, data)
        (tree / "empty directory").mkdir()
        self.assert_ok(self.run_bcmr("copy", "-r", f"legacy:{tree}", self.dest))
        self.assert_files(self.dest / tree.name, contents)
        self.assertTrue((self.dest / tree.name / "empty directory").is_dir())
        self.assertFalse((self.home / "INJECTED").exists())

    # A genuinely empty directory is valid and remains distinguishable from
    # truncated or unframed stdout from an incompatible remote command.
    def test_portable_empty_directory_remains_empty(self):
        self.force_portable_find()
        tree = self.home / "empty"
        tree.mkdir()
        self.assert_ok(self.run_bcmr("copy", "-r", f"legacy:{tree}", self.dest))
        self.assertTrue((self.dest / "empty").is_dir())
        self.assertEqual(list((self.dest / "empty").iterdir()), [])

    # This is malformed output at the external command boundary, not a fake
    # implementation of the production parser.
    def test_successful_find_with_malformed_stdout_is_an_error(self):
        self.put(self.home, "tree/leaf.txt", "must not disappear")
        command = self.bin / "find"
        command.write_text("#!/bin/sh\nprintf 'not a NUL framed listing'\n")
        command.chmod(0o755)
        result = self.run_bcmr("copy", "-r", f"legacy:{self.home / 'tree'}", self.dest)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_files(self.dest, {})

    # A failed per-entry stat must fail the whole listing, not look empty.
    def test_portable_listing_propagates_stat_failure(self):
        self.force_portable_find()
        self.put(self.home, "tree/STAT_FAIL", "unreadable metadata fixture")
        command = self.bin / "stat"
        command.write_text("""#!/bin/sh
for arg do
    case "$arg" in *STAT_FAIL) printf 'fixture stat failure\n' >&2; exit 1;; esac
done
exec /usr/bin/stat "$@"
""")
        command.chmod(0o755)
        result = self.run_bcmr("copy", "-r", f"legacy:{self.home / 'tree'}", self.dest)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("stat failure", result.stderr)
        self.assert_files(self.dest, {})


if __name__ == "__main__":
    unittest.main(verbosity=2)
