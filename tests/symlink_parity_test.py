#!/usr/bin/env python3
"""Link identity and dereferencing through real CLI, shells and serve processes.

Only the external SSH boundary is replaced by the shared fixture. Run with
BCMR=/absolute/path/to/bcmr python3 tests/symlink_parity_test.py.
"""
import os
from pathlib import Path
import struct
import subprocess
import unittest

import destination_semantics_test as destinations
import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX links and shell")
class SymlinkParityTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    route = destinations.DestinationSemanticsTests.route
    MODES = destinations.DestinationSemanticsTests.MODES

    def test_copy_follows_file_links_using_target_size(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                tree = src / "tree"
                self.put(tree, "payload", "target bytes " * 23)
                (tree / "file-link").symlink_to("payload")
                self.assert_ok(self.run_bcmr("copy", "-r", source(tree), target(out)))
                landed = out / "tree/file-link"
                self.assertFalse(landed.is_symlink())
                self.assertEqual(landed.read_text(), "target bytes " * 23)

    def test_copy_follows_directory_links_recursively(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                tree = src / "tree"
                tree.mkdir()
                self.put(src, "external/deep/payload", "through directory link")
                (tree / "directory-link").symlink_to("../external", target_is_directory=True)
                self.assert_ok(self.run_bcmr("copy", "-r", source(tree), target(out)))
                self.assertFalse((out / "tree/directory-link").is_symlink())
                self.assertTrue((out / "tree/directory-link/deep/payload").is_file())
                self.assertEqual((out / "tree/directory-link/deep/payload").read_text(),
                                 "through directory link")

    def test_repeated_directory_aliases_are_not_ancestor_cycles(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                tree = src / "tree"
                tree.mkdir()
                self.put(src, "shared/payload", "both aliases")
                for name in ("first", "second"):
                    (tree / name).symlink_to("../shared", target_is_directory=True)
                self.assert_ok(self.run_bcmr("copy", "-r", source(tree), target(out)))
                for name in ("first", "second"):
                    self.assertTrue((out / "tree" / name / "payload").is_file())
                    self.assertEqual((out / "tree" / name / "payload").read_text(), "both aliases")

    def test_copy_no_deref_preserves_file_directory_and_dangling_links(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                tree = src / "tree"
                self.put(tree, "payload", "kept regular file")
                self.put(src, "external/payload", "never copied through link")
                links = {"file-link": "payload", "directory-link": "../external",
                         "dangling-link": "missing target ' $(literal)\nnext"}
                for name, value in links.items():
                    (tree / name).symlink_to(value, target_is_directory=name == "directory-link")
                self.assert_ok(self.run_bcmr("copy", "-r", "--no-deref", source(tree), target(out)))
                for name, value in links.items():
                    landed = out / "tree" / name
                    self.assertTrue(landed.is_symlink(), str(landed))
                    self.assertEqual(os.readlink(landed), value)
                self.assertEqual((out / "tree/payload").read_text(), "kept regular file")

    def test_top_level_dangling_link_is_preserved_without_recursive_flag(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                link = src / "link"
                link.symlink_to("missing target")
                self.assert_ok(self.run_bcmr("copy", "--no-deref", source(link), target(out)))
                self.assertEqual(os.readlink(out / "link"), "missing target")

    def test_selected_cycles_and_broken_targets_fail_in_default_copy(self):
        for mode in self.MODES:
            for name, value in (("cycle", "."), ("broken", "missing-target")):
                with self.subTest(mode=mode, link=name):
                    src, out, source, target = self.route(mode + "-" + name)
                    # Preserve the route's host/operation classification.
                    host = "legacy" if mode.endswith("legacy") else "fixture"
                    source = lambda path: f"{host}:{path}" if mode.startswith("download") else str(path)
                    target = lambda path: f"{host}:{path}" if mode.startswith("upload") else str(path)
                    tree = src / "tree"
                    tree.mkdir()
                    (tree / name).symlink_to(value, target_is_directory=name == "cycle")
                    result = self.run_bcmr("copy", "-r", source(tree), target(out))
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_excluded_link_is_pruned_before_following(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                tree = src / "tree"
                self.put(tree, "keep", "selected bytes")
                (tree / "omit").symlink_to(".", target_is_directory=True)
                self.assert_ok(self.run_bcmr("copy", "-r", "--exclude", r"(^|/)omit$",
                                             source(tree), target(out)))
                self.assertEqual((out / "tree/keep").read_text(), "selected bytes")
                self.assertFalse((out / "tree/omit").exists())

    def test_move_preserves_link_identity_and_linked_target(self):
        for mode in self.MODES:
            with self.subTest(mode=mode):
                src, out, source, target = self.route(mode)
                tree = src / "tree"
                tree.mkdir()
                external = self.put(src, "external/payload", "never removed")
                (tree / "directory-link").symlink_to("../external", target_is_directory=True)
                (tree / "dangling").symlink_to("absent")
                self.assert_ok(self.run_bcmr("move", "-r", source(tree), target(out)))
                self.assertTrue((out / "tree/directory-link").is_symlink())
                self.assertEqual(os.readlink(out / "tree/dangling"), "absent")
                self.assertFalse(tree.exists())
                self.assertEqual(external.read_text(), "never removed")

    def test_source_separators_preserve_directory_link_identity(self):
        # A terminal separator must not turn lstat/rename into operations on
        # the referent, including the copy plus cleanup paths used by SSH.
        for mode in self.MODES:
            for operation in ("copy", "move"):
                with self.subTest(mode=mode, operation=operation):
                    src, out, source, target = self.route(mode + "-" + operation)
                    # route suffixes above label fixtures; transport follows mode.
                    host = "legacy" if mode.endswith("legacy") else "fixture"
                    source = lambda path: f"{host}:{path}" if mode.startswith("download") else str(path)
                    target = lambda path: f"{host}:{path}" if mode.startswith("upload") else str(path)
                    referent = src / "referent"
                    payload = self.put(referent, "payload", "link target must survive")
                    link = src / "link"
                    link.symlink_to(referent, target_is_directory=True)
                    options = ["--no-deref"] if operation == "copy" else []
                    result = self.run_bcmr(operation, "-r", *options,
                                           source(link) + "///", target(out))
                    self.assert_ok(result)
                    landed = out / "link"
                    self.assertTrue(landed.is_symlink(), result.stdout + result.stderr)
                    self.assertEqual(os.readlink(landed), str(referent))
                    self.assertEqual(payload.read_text(), "link target must survive")
                    self.assertEqual(link.is_symlink(), operation == "copy")

    def test_source_separators_preserve_dangling_link_without_recursion(self):
        for operation in ("copy", "move"):
            with self.subTest(operation=operation):
                link = self.local / operation / "link"
                link.parent.mkdir()
                link.symlink_to("missing-target")
                destination = self.dest / operation
                options = ["--no-deref"] if operation == "copy" else []
                self.assert_ok(self.run_bcmr(operation, *options, str(link) + "/", destination))
                self.assertEqual(os.readlink(destination), "missing-target")
                self.assertEqual(link.is_symlink(), operation == "copy")

    def test_fan_out_source_separators_preserve_link_target(self):
        for remote_source in (False, True):
            with self.subTest(remote_source=remote_source):
                label = "remote" if remote_source else "local"
                root = (self.home if remote_source else self.local) / label
                payload = self.put(root, "referent/payload", "fan-out referent survives")
                link = root / "link"
                link.symlink_to(root / "referent", target_is_directory=True)
                source = f"fixture:{link}/" if remote_source else str(link) + "/"
                local_target = self.dest / label
                remote_target = self.second / label
                self.assert_ok(self.run_bcmr("move", "-r", source,
                                             "--to", str(local_target) + "/",
                                             "--to", f"second:{remote_target}/"))
                for destination in (local_target / "link", remote_target / "link"):
                    self.assertTrue(destination.is_symlink(), str(destination))
                    self.assertEqual(os.readlink(destination), str(root / "referent"))
                self.assertFalse(link.is_symlink())
                self.assertEqual(payload.read_text(), "fan-out referent survives")

    def test_source_separator_exclusion_uses_original_spelling(self):
        payload = self.put(self.local, "referent/payload", "excluded referent")
        link = self.local / "link"
        link.symlink_to(self.local / "referent", target_is_directory=True)
        self.assert_ok(self.run_bcmr("move", "-r", "--exclude", r"/link/$",
                                     str(link) + "/", self.dest))
        self.assertTrue(link.is_symlink())
        self.assertEqual(payload.read_text(), "excluded referent")
        self.assertEqual(list(self.dest.iterdir()), [])

    def test_source_separator_trimming_keeps_colon_link_local(self):
        for operation in ("copy", "move"):
            with self.subTest(operation=operation):
                payload = self.put(self.local, operation + "-target/payload", "local colon link")
                link = self.local / ("fixture:" + operation + "-link")
                link.symlink_to(payload.parent, target_is_directory=True)
                options = ["--no-deref"] if operation == "copy" else []
                destination = self.dest / operation
                self.assert_ok(self.run_bcmr(operation, "-r", *options,
                                             "./" + link.name + "/", destination))
                self.assertTrue(destination.is_symlink())
                self.assertEqual(os.readlink(destination), str(payload.parent))
                self.assertEqual(payload.read_text(), "local colon link")

    def test_ordinary_directory_source_separators_keep_basename_rules(self):
        for operation in ("copy", "move"):
            with self.subTest(operation=operation):
                directory = self.local / operation / "tree"
                self.put(directory, "payload", "ordinary directory")
                destination = self.dest / operation
                self.assert_ok(self.run_bcmr(operation, "-r", str(directory) + "///",
                                             str(destination) + "/"))
                self.assertEqual((destination / "tree/payload").read_text(), "ordinary directory")

    def test_serve_list_rejects_a_followed_target_outside_its_root(self):
        jail = self.root / "jail"
        jail.mkdir()
        self.put(self.root, "outside/secret", "outside bytes")
        (jail / "escape").symlink_to(self.root / "outside", target_is_directory=True)

        def frame(payload):
            return struct.pack("<I", len(payload)) + payload

        name = b"."
        request = frame(b"\x01\x02\x00") + frame(b"\x02" + struct.pack("<I", len(name)) + name)
        result = subprocess.run([fixture.BCMR, "serve", "--root", str(jail)], input=request,
                                env=self.env, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = result.stdout
        first_len = struct.unpack("<I", data[:4])[0]
        second = data[4 + first_len:]
        self.assertGreaterEqual(len(second), 5)
        self.assertEqual(second[4], 0x83, "LIST must return Error for an escaped link")
        self.assertIn(b"root", second.lower())

    def test_remote_inventory_probe_count_depends_on_directories_not_regular_files(self):
        probes = self.root / "tree-probes"
        ssh = self.bin / "ssh"
        script = ssh.read_text()
        marker = "# OpenSSH joins command arguments and executes them with the login shell."
        self.assertIn(marker, script)
        script = script.replace(marker, '''
if 'bcmr-tree' in ' '.join(words):
    with open(os.environ['TREE_PROBE_COUNT'], 'a') as stream:
        stream.write('probe\\n')
''' + marker)
        ssh.write_text(script)
        self.env["TREE_PROBE_COUNT"] = str(probes)
        counts = {}
        for files in (2, 30):
            tree = self.home / f"tree-{files}"
            names = ("left 'quoted\nline", "right")
            for directory in names:
                (tree / directory).mkdir(parents=True)
            expected = {}
            for number in range(files):
                name = f"{names[number % 2]}/file '{number}\n.txt"
                expected[name] = f"payload {number}"
                self.put(tree, name, expected[name])
            before = len(probes.read_text().splitlines()) if probes.exists() else 0
            self.assert_ok(self.run_bcmr("copy", "-r", f"fixture:{tree}", self.dest))
            counts[files] = len(probes.read_text().splitlines()) - before
            for name, content in expected.items():
                self.assertEqual((self.dest / tree.name / name).read_text(), content)
        self.assertLessEqual(counts[30], counts[2] + 2,
                             f"tree probes must not scale with regular leaves: {counts}")
        self.assertLessEqual(counts[30], 12,
                             f"two subdirectories should need bounded directory probes: {counts}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
