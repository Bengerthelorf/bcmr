#!/usr/bin/env python3
"""Config recovery regressions against a built binary; set BCMR to select it."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

BCMR = Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve()


@unittest.skipIf(os.name == "nt", "tests isolate Unix HOME config locations")
class ConfigDoctorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bcmr config ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, HOME=str(self.root),
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_CACHE_HOME=str(self.root / "cache"),
                        XDG_DATA_HOME=str(self.root / "data"))
        self.env.pop("BCMR_CONFIG", None)
        self.env.pop("BCMR_PROFILE", None)

    def platform_config_dir(self):
        if sys.platform == "darwin":
            return "Library/Application Support/com.bcmr.bcmr"
        return "config/bcmr"

    def write_config(self, relative, content):
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        return path

    def run_bcmr(self, *args):
        return subprocess.run([str(BCMR), *map(str, args)], cwd=self.root,
                              env=self.env, capture_output=True, text=True,
                              timeout=20)

    def config_check(self, *args):
        result = self.run_bcmr("--json", *args, "doctor")
        self.assertIn(result.returncode, (0, 1), result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")
        report = json.loads(result.stdout)
        checks = [check for check in report["local"]
                  if check["label"] == "config file"]
        self.assertEqual(len(checks), 1, report)
        return result, report, checks[0]

    def test_help_and_versions_recover_from_malformed_config(self):
        path = self.write_config("bad.toml", "[unterminated\n")
        self.env["BCMR_CONFIG"] = str(path)
        for args in (("--help",), ("-h",), ("copy", "--help"),
                     ("help", "copy"), ("--version",),
                     ("copy", "--version")):
            with self.subTest(args=args):
                result = self.run_bcmr(*args)
                self.assertEqual(result.returncode, 0,
                                 result.stdout + result.stderr)
                self.assertEqual(result.stderr, "")
                self.assertIn("bcmr", result.stdout)

    def test_help_ignores_missing_explicit_config(self):
        result = self.run_bcmr("--config", self.root / "missing.toml",
                               "copy", "--help")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Usage:", result.stdout)

    def test_shell_setup_commands_ignore_malformed_config(self):
        for discovered in (False, True):
            relative = ".config/bcmr/config.toml" if discovered else "bad.toml"
            path = self.write_config(relative, "[unterminated\n")
            if discovered:
                self.env.pop("BCMR_CONFIG", None)
            else:
                self.env["BCMR_CONFIG"] = str(path)
            for command in ("init", "completions"):
                for shell in ("zsh", "bash"):
                    with self.subTest(discovered=discovered,
                                      command=command, shell=shell):
                        result = self.run_bcmr(command, shell)
                        self.assertEqual(result.returncode, 0,
                                         result.stdout + result.stderr)
                        self.assertEqual(result.stderr, "")
                        self.assertIn("bcmr", result.stdout)
            path.unlink()

    def test_shell_setup_commands_ignore_invalid_explicit_config(self):
        malformed = self.write_config("bad.toml", "[unterminated\n")
        for path in (self.root / "missing.toml", malformed):
            for command in ("init", "completions"):
                for shell in ("zsh", "bash"):
                    with self.subTest(path=path, command=command, shell=shell):
                        result = self.run_bcmr("--config", path, command, shell)
                        self.assertEqual(result.returncode, 0,
                                         result.stdout + result.stderr)
                        self.assertEqual(result.stderr, "")
                        self.assertIn("bcmr", result.stdout)

    def test_doctor_reports_malformed_config_as_json(self):
        path = self.write_config("bad.yaml", "progress: [unterminated\n")
        for explicit in (False, True):
            with self.subTest(explicit=explicit):
                self.env["BCMR_CONFIG"] = str(path)
                args = ("--config", path) if explicit else ()
                result, report, check = self.config_check(*args)
                self.assertEqual(result.returncode, 1, report)
                self.assertFalse(report["ok"], report)
                self.assertEqual(check["status"], "fail", check)
                self.assertIn(str(path), check["detail"])
                self.assertIn("error", check["detail"].lower())

    def test_doctor_accepts_explicit_yaml_config(self):
        path = self.write_config("custom.yaml", "progress:\n  style: off\n")
        result, report, check = self.config_check("--config", path)
        self.assertEqual(result.returncode, 0, report)
        self.assertEqual(check["status"], "ok", check)
        self.assertIn(str(path), check["detail"])

    def test_doctor_checks_loaded_yaml_and_schema(self):
        path = self.write_config(".config/bcmr/config.yaml",
                                 "scp:\n  parallel_transfers: 0\n")
        result, report, check = self.config_check()
        self.assertEqual(result.returncode, 1, report)
        self.assertFalse(report["ok"], report)
        self.assertEqual(check["status"], "fail", check)
        self.assertIn(str(path), check["detail"])
        self.assertIn("parallel_transfers", check["detail"])

    def test_doctor_keeps_discovered_layers_with_explicit_override(self):
        legacy = self.write_config(".config/bcmr/config.toml",
                                   '[progress]\nstyle = "plain"\n')
        override = self.write_config("override.yaml", "progress:\n  style: off\n")
        result, report, check = self.config_check("--config", override)
        self.assertEqual(result.returncode, 0, report)
        self.assertEqual(check["status"], "ok", check)
        self.assertIn(str(legacy), check["detail"])
        self.assertIn(str(override), check["detail"])
        self.assertLess(check["detail"].index(str(legacy)),
                        check["detail"].index(str(override)))

    def test_bad_config_still_stops_normal_operations(self):
        path = self.write_config("bad.toml", "[unterminated\n")
        self.env["BCMR_CONFIG"] = str(path)
        result = self.run_bcmr("copy", "missing", "destination")
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("failed to parse configuration", result.stderr)
        self.assertFalse((self.root / "destination").exists())

    def test_literal_config_filename_after_separator_copies_normally(self):
        (self.root / "--config").write_text("literal config filename\n")
        result = self.run_bcmr("copy", "--", "--config", "output.txt")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.root / "output.txt").read_text(),
                         "literal config filename\n")

    def test_literal_profile_filename_after_separator_copies_normally(self):
        for filename in ("--profile", "--profile=literal"):
            with self.subTest(filename=filename):
                (self.root / filename).write_text("literal profile filename\n")
                result = self.run_bcmr("copy", "--", filename, "output.txt")
                self.assertEqual(result.returncode, 0,
                                 result.stdout + result.stderr)
                self.assertEqual((self.root / "output.txt").read_text(),
                                 "literal profile filename\n")
                (self.root / "output.txt").unlink()

    def test_literal_filename_does_not_override_profile_recursive_default(self):
        config = self.write_config("profile.toml",
                                   '[profile.tree]\ndefault_args = ["--recursive"]\n')
        self.env["BCMR_CONFIG"] = str(config)
        (self.root / "--recursive").mkdir()
        (self.root / "--recursive/payload").write_text("profile keeps recursive\n")
        result = self.run_bcmr("--profile", "tree", "copy", "--",
                               "--recursive", "output")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.root / "output/payload").read_text(),
                         "profile keeps recursive\n")

    def test_tokens_after_separator_are_not_help_or_version(self):
        path = self.write_config("bad.toml", "[unterminated\n")
        self.env["BCMR_CONFIG"] = str(path)
        for token in ("--help", "--version"):
            with self.subTest(token=token):
                result = self.run_bcmr("copy", "--", token, "destination")
                self.assertEqual(result.returncode, 2,
                                 result.stdout + result.stderr)
                self.assertIn("failed to parse configuration", result.stderr)

    def test_version_like_option_value_is_not_a_version_request(self):
        path = self.write_config("bad.toml", "[unterminated\n")
        self.env["BCMR_CONFIG"] = str(path)
        for args in (("copy", "--to=--version", "missing"),
                     ("copy", "--to", "--version", "missing"),
                     ("copy", "--to=--help", "missing")):
            with self.subTest(args=args):
                result = self.run_bcmr(*args)
                self.assertEqual(result.returncode, 2,
                                 result.stdout + result.stderr)
                self.assertIn("failed to parse configuration", result.stderr)

    def test_doctor_checks_platform_config_directory(self):
        path = self.write_config(self.platform_config_dir() + "/config.yaml",
                                 "progress:\n  style: plain\n")
        result, report, check = self.config_check()
        self.assertEqual(result.returncode, 0, report)
        self.assertEqual(check["status"], "ok", check)
        self.assertIn(str(path), check["detail"])

    def test_later_layer_can_override_schema_error(self):
        legacy = self.write_config(".config/bcmr/config.toml",
                                   "[scp]\nparallel_transfers = 0\n")
        override = self.write_config("override.toml",
                                     "[scp]\nparallel_transfers = 2\n")
        result, report, check = self.config_check("--config", override)
        self.assertEqual(result.returncode, 0, report)
        self.assertEqual(check["status"], "ok", check)
        self.assertIn(str(legacy), check["detail"])
        self.assertIn(str(override), check["detail"])

    def test_missing_override_is_reported_as_ignored(self):
        path = self.root / "missing.toml"
        self.env["BCMR_CONFIG"] = str(path)
        result, report, check = self.config_check()
        self.assertEqual(result.returncode, 0, report)
        self.assertTrue(report["ok"], report)
        self.assertEqual(check["status"], "warn", check)
        self.assertIn(str(path), check["detail"])
        self.assertIn("ignored", check["detail"])

    def test_config_layers_keep_their_existing_precedence(self):
        toml = self.write_config(".config/bcmr/config.toml",
                                 '[paths]\noutput = "from-toml"\n')
        yaml = self.write_config(".config/bcmr/config.yaml",
                                 'paths:\n  output: from-yaml\n')
        platform = self.write_config(self.platform_config_dir() + "/config.toml",
                                     '[paths]\noutput = "from-platform"\n')
        override = self.write_config("override.toml",
                                     '[paths]\noutput = "from-override"\n')
        self.env["BCMR_CONFIG"] = str(override)
        (self.root / "payload").write_text("layered copy\n")
        for destination, removed_after in (("from-override", override),
                                           ("from-platform", platform),
                                           ("from-yaml", yaml),
                                           ("from-toml", toml)):
            with self.subTest(destination=destination):
                result = self.run_bcmr("copy", "payload", "@output")
                self.assertEqual(result.returncode, 0,
                                 result.stdout + result.stderr)
                self.assertEqual((self.root / destination).read_text(),
                                 "layered copy\n")
                removed_after.unlink()

    def test_profile_defaults_still_apply_to_normal_copy(self):
        config = self.write_config("profile.toml",
                                   '[profile.tree]\ndefault_args = ["--recursive"]\n')
        self.env["BCMR_CONFIG"] = str(config)
        (self.root / "source").mkdir()
        (self.root / "source/payload").write_text("copied by profile\n")
        result = self.run_bcmr("--profile", "tree", "copy", "source", "destination")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.root / "destination/payload").read_text(),
                         "copied by profile\n")


if __name__ == "__main__":
    unittest.main()
