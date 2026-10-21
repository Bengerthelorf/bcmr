#!/usr/bin/env python3
"""Legacy recursive-listing regressions; only SSH and tool failures are fixtures.

Run with BCMR=/absolute/path/to/bcmr python3 tests/remote_listing_test.py.
"""
import os
from pathlib import Path
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires a POSIX shell and filesystem")
class RemoteListingTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    assert_files = fixture.SourceStarTests.assert_files

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
