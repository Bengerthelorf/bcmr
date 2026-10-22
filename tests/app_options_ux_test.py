#!/usr/bin/env python3
"""Shared CLI defaults, dry-run confirmation, and remote removal contracts."""
import json
import os
import subprocess
import unittest

import star_copy_test as fixture


@unittest.skipUnless(os.name == "posix", "requires POSIX SSH fixture")
class AppOptionsUxTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    assert_ok = fixture.SourceStarTests.assert_ok

    def run_cli(self, *args, terminal_answer=None):
        command = [fixture.BCMR, *map(str, args)]
        if terminal_answer is None:
            return subprocess.run(command, cwd=self.local, env=self.env,
                                  stdin=subprocess.DEVNULL, capture_output=True,
                                  text=True, timeout=30)
        import pty
        master, slave = pty.openpty()
        try:
            child = subprocess.Popen(command, cwd=self.local, env=self.env,
                                     stdin=slave, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True)
            os.close(slave)
            slave = None
            os.write(master, terminal_answer.encode())
            stdout, stderr = child.communicate(timeout=30)
            return subprocess.CompletedProcess(command, child.returncode, stdout, stderr)
        finally:
            if slave is not None:
                os.close(slave)
            os.close(master)

    def configure(self, text):
        path = self.root / "options.toml"
        path.write_text(text)
        self.env["BCMR_CONFIG"] = str(path)

    def test_short_and_long_cli_options_override_profile_spelling(self):
        source = self.put(self.local, "source", "copied bytes")
        for index, (default, explicit) in enumerate((
                ('["--jobs", "4", "-p"]', ["-j1", "--preserve"]),
                ('["-j4", "--preserve"]', ["--jobs", "1", "-p"]),
                ('["-rp"]', ["-p", "-r"]),
        )):
            with self.subTest(default=default, explicit=explicit):
                self.configure(f'[profile.work]\ndefault_args = {default}\n')
                destination = self.dest / f"copy-{index}"
                result = self.run_cli("--profile", "work", "copy", "--progress=off",
                                      *explicit, source, destination)
                self.assert_ok(result)
                self.assertEqual(destination.read_text(), "copied bytes")

    def test_host_defaults_override_profile_defaults(self):
        self.configure('[profile.work]\ndefault_args = ["--jobs", "4"]\n'
                       '[host.fixture]\ndefault_args = ["-j", "2"]\n')
        source = self.put(self.local, "source", "host default bytes")
        self.assert_ok(self.run_cli("--profile", "work", "copy", "--progress=off",
                                    source, "fixture:copied"))
        self.assertEqual((self.home / "copied").read_text(), "host default bytes")

    def test_non_path_option_value_does_not_select_a_host_profile(self):
        self.configure('[host.fixture]\ndefault_args = ["-P", "2"]\n')
        source = self.put(self.local, "source", "local move bytes")
        destination = self.dest / "moved"
        result = self.run_cli("move", "--progress=off", "--exclude", "fixture:unmatched",
                              source, destination)
        self.assert_ok(result)
        self.assertEqual(destination.read_text(), "local move bytes")

    def test_move_preflights_every_dot_source_before_any_destination_write(self):
        for index, suffix in enumerate((".", "../", "./")):
            with self.subTest(suffix=suffix):
                directory = self.local / f"tree-{index}"
                payload = self.put(directory, "payload", "tree bytes")
                first = self.put(self.local, f"first-{index}", "first source bytes")
                destination = self.dest / str(index)
                destination.mkdir()
                result = self.run_cli("move", "-r", "--progress=off", first,
                                      str(directory) + "/" + suffix, destination)
                self.assertTrue(first.exists(), result.stdout + result.stderr)
                self.assertTrue(payload.exists(), result.stdout + result.stderr)
                self.assertEqual(list(destination.iterdir()), [])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("named source", result.stderr)

    def test_move_dot_source_is_rejected_for_remote_upload_download_and_fanout(self):
        for kind in ("upload", "download", "fanout"):
            with self.subTest(kind=kind):
                local = self.put(self.local, f"{kind}/payload", "local tree bytes")
                remote = self.put(self.home, f"{kind}/payload", "remote tree bytes")
                destination = self.dest / kind
                if kind == "upload":
                    paths = (str(local.parent) + "/.", "fixture:uploaded")
                elif kind == "download":
                    paths = (f"fixture:{kind}/.", destination)
                else:
                    paths = (str(local.parent) + "/.", "--to", destination)
                result = self.run_cli("move", "-r", "--progress=off", *paths)
                self.assertTrue(local.exists(), result.stdout + result.stderr)
                self.assertTrue(remote.exists(), result.stdout + result.stderr)
                self.assertFalse((self.home / "uploaded").exists())
                self.assertFalse(destination.exists())
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("named source", result.stderr)

    def test_move_named_source_with_parent_component_remains_valid(self):
        source = self.put(self.root, "sibling", "named source bytes")
        destination = self.dest / "moved"
        self.assert_ok(self.run_cli("move", "--progress=off", "../sibling", destination))
        self.assertFalse(source.exists())
        self.assertEqual(destination.read_text(), "named source bytes")

    def test_json_force_dry_run_needs_no_confirmation_and_changes_nothing(self):
        source = self.put(self.local, "source", "source bytes")
        destination = self.put(self.dest, "existing", "destination bytes")
        for operation in ("copy", "move"):
            with self.subTest(operation=operation):
                result = self.run_cli("--json", operation, "-nf", source, destination)
                self.assert_ok(result)
                self.assert_json_success(result)
                self.assertEqual(source.read_text(), "source bytes")
                self.assertEqual(destination.read_text(), "destination bytes")
        result = self.run_cli("--json", "remove", "-ni", source)
        self.assert_ok(result)
        self.assert_json_success(result)
        self.assertTrue(source.exists())

    def assert_json_success(self, result):
        events = []
        for line in result.stdout.splitlines():
            try:
                events.append(json.loads(line))
            except ValueError:
                self.fail(f"non-JSON output: {line!r}")
        self.assertTrue(events, "JSON operation must emit a terminal result")
        results = [event for event in events if event.get("type") == "result"]
        self.assertEqual(len(results), 1, result.stdout)
        self.assertEqual(events[-1], results[0])
        self.assertEqual(results[0]["status"], "success")

    def assert_json_dry_run(self, operation, *args):
        def snapshot(base):
            entries = {}
            for path in base.rglob("*"):
                name = str(path.relative_to(base))
                if path.is_symlink():
                    entries[name] = ("link", os.readlink(path))
                elif path.is_file():
                    entries[name] = ("file", path.read_bytes())
                elif path.is_dir():
                    entries[name] = ("directory",)
            return entries

        roots = (self.local, self.home, self.second, self.dest)
        before = [snapshot(base) for base in roots]
        result = self.run_cli("--json", operation, "-nf", *args)
        self.assert_ok(result)
        self.assertEqual([snapshot(base) for base in roots], before)
        self.assert_json_success(result)

    def test_json_remote_only_remove_emits_terminal_result(self):
        for index, flags in enumerate(("-f", "-nf")):
            with self.subTest(flags=flags):
                source = self.put(self.home, f"source-{index}", "remote source bytes")
                result = self.run_cli("--json", "remove", flags, f"fixture:{source.name}")
                self.assert_ok(result)
                self.assert_json_success(result)
                if flags == "-nf":
                    self.assertEqual(source.read_text(), "remote source bytes")
                else:
                    self.assertFalse(source.exists())

    def test_json_remote_force_dry_run_upload_and_download(self):
        local = self.put(self.local, "source", "local source bytes")
        self.put(self.home, "source", "remote source bytes")
        self.put(self.home, "target", "remote destination bytes")
        destination = self.put(self.dest, "target", "local destination bytes")
        for operation in ("copy", "move"):
            for direction, paths in (("upload", (local, "fixture:target")),
                                     ("download", ("fixture:source", destination))):
                with self.subTest(operation=operation, direction=direction):
                    self.assert_json_dry_run(operation, *paths)

    def test_json_remote_link_force_dry_run_upload_and_download(self):
        self.put(self.local, "referent", "local referent bytes")
        local = self.local / "source-link"
        local.symlink_to("referent")
        self.put(self.home, "referent", "remote referent bytes")
        (self.home / "source-link").symlink_to("referent")
        self.put(self.home, "target", "remote destination bytes")
        destination = self.put(self.dest, "target", "local destination bytes")
        for operation in ("copy", "move"):
            for direction, paths in (("upload", (local, "fixture:target")),
                                     ("download", ("fixture:source-link", destination))):
                with self.subTest(operation=operation, direction=direction):
                    self.assert_json_dry_run(operation, "--no-deref", *paths)

    def test_json_composite_force_dry_run_mixed_sources_and_relay(self):
        local = self.put(self.local, "local-source", "local source bytes")
        self.put(self.home, "remote-source", "remote source bytes")
        for operation in ("copy", "move"):
            for kind, paths in (("mixed", (local, "fixture:remote-source", self.dest)),
                                ("relay", ("fixture:remote-source", "second:target"))):
                with self.subTest(operation=operation, kind=kind):
                    self.assert_json_dry_run(operation, *paths)

    def test_json_real_overwrite_still_requires_explicit_yes(self):
        source = self.put(self.local, "source", "source bytes")
        destination = self.put(self.dest, "existing", "destination bytes")
        for operation in ("copy", "move"):
            with self.subTest(operation=operation):
                result = self.run_cli("--json", operation, "-f", source, destination)
                self.assertNotEqual(result.returncode, 0)
                events = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertEqual(events[-1]["error_kind"], "confirmation_required")
                self.assertEqual(source.read_text(), "source bytes")
                self.assertEqual(destination.read_text(), "destination bytes")

    def test_json_remove_yes_or_force_disables_interactive_prompts(self):
        for flags in ("-iy", "-if"):
            with self.subTest(flags=flags):
                source = self.put(self.local, "source", "remove these bytes")
                result = self.run_cli("--json", "remove", flags, source)
                self.assert_ok(result)
                self.assertFalse(source.exists())
                events = [json.loads(line) for line in result.stdout.splitlines()]
                self.assertEqual(events[-1]["status"], "success")

    def test_remote_exclusion_rejected_before_any_local_or_remote_deletion(self):
        first = self.put(self.home, "first/keep.txt", "first remote bytes")
        second = self.put(self.second, "second/keep.txt", "second remote bytes")
        local = self.put(self.local, "local.txt", "local bytes")
        result = self.run_cli("remove", "--progress=off", "-rf", "--exclude", "keep",
                              "fixture:first", local, "second:second")
        self.assertTrue(first.exists(), result.stdout + result.stderr)
        self.assertTrue(second.exists(), result.stdout + result.stderr)
        self.assertTrue(local.exists(), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--exclude", result.stderr)
        self.assertIn("remote", result.stderr.lower())

    def test_remote_interactive_is_not_silently_reduced_to_one_batch_prompt(self):
        source = self.put(self.home, "tree/one.txt", "remote bytes")
        result = self.run_cli("remove", "--progress=off", "-ri", "fixture:tree",
                              terminal_answer="y\n")
        self.assertTrue(source.exists(), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--interactive", result.stderr)

    def test_mixed_remove_preflights_local_operands_before_remote_deletion(self):
        for index, (flags, suffix) in enumerate((("-rf", "/."), ("-ry", "/missing"),
                                                  ("-y", ""))):
            with self.subTest(flags=flags, suffix=suffix):
                remote = self.put(self.home, f"remote-{index}", "preserve remote bytes")
                local = self.put(self.local, f"tree-{index}/payload", "preserve local bytes")
                result = self.run_cli("remove", "--progress=off", flags,
                                      f"fixture:{remote.name}", str(local.parent) + suffix)
                self.assertTrue(remote.exists(), result.stdout + result.stderr)
                self.assertTrue(local.exists(), result.stdout + result.stderr)
                self.assertNotEqual(result.returncode, 0)

    def test_remote_dot_operand_is_rejected_before_other_remote_deletion(self):
        first = self.put(self.home, "first", "first remote bytes")
        payload = self.put(self.home, "tree/payload", "remote tree bytes")
        result = self.run_cli("remove", "--progress=off", "-rf",
                              "fixture:first", "fixture:tree/./")
        self.assertTrue(first.exists(), result.stdout + result.stderr)
        self.assertTrue(payload.exists(), result.stdout + result.stderr)
        self.assertNotEqual(result.returncode, 0)

    def test_remote_quiet_suppresses_success_even_with_verbose(self):
        source = self.put(self.home, "source", "remote bytes")
        result = self.run_cli("remove", "-fqv", "fixture:source")
        self.assert_ok(result)
        self.assertFalse(source.exists())
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
