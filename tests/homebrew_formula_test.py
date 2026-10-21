#!/usr/bin/env python3
"""Exercise the generated formula's install method using real Homebrew."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
BCMR = Path(os.environ.get("BCMR", ROOT / "target/debug/bcmr")).resolve()
ASSETS = (
    "bcmr-aarch64-macos.tar.gz",
    "bcmr-x86_64-macos.tar.gz",
    "bcmr-aarch64-linux.tar.gz",
    "bcmr-x86_64-linux.tar.gz",
)


class HomebrewFormulaTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="bcmr brew ")
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.manifest = self.work / "sha256sums.txt"
        self.manifest.write_text("".join(f"{'a' * 64}  {asset}\n" for asset in ASSETS))

    def render(self, version="v0.7.0"):
        return subprocess.run(
            ["bash", str(ROOT / "scripts/render-homebrew-formula.sh"), version, str(self.manifest)],
            text=True, capture_output=True, check=False,
        )

    def test_installs_completions(self):
        """Removing completion installation leaves the installed CLI incomplete."""
        if not shutil.which("brew"):
            self.skipTest("Homebrew is required for the formula install test")
        formula = self.work / "bcmr.rb"
        supplied = os.environ.get("BCMR_FORMULA")
        if supplied:
            formula.write_bytes(Path(supplied).read_bytes())
        else:
            result = self.render()
            self.assertEqual(result.returncode, 0, result.stderr)
            formula.write_text(result.stdout)
        stage = self.work / "stage"
        stage.mkdir()
        shutil.copy2(BCMR, stage / "bcmr")
        prefix = self.work / "installed"
        env = {**os.environ, "HOMEBREW_NO_AUTO_UPDATE": "1", "HOMEBREW_DEVELOPER": "1"}
        # Installing shell scripts must also work when an existing user config
        # needs repair; Homebrew executes this binary to generate completions.
        invalid_config = self.work / "broken.toml"
        invalid_config.write_text("[unterminated\n")
        ruby = """
require 'formula'
require 'formulary'
# Homebrew filters custom environment variables at startup. Set the fixture
# inside Ruby so the actual completion-generation subprocess receives it.
ENV['BCMR_CONFIG'] = ARGV.fetch(3)
f = Formulary.factory(ARGV.fetch(0))
f.define_singleton_method(:prefix) { Pathname(ARGV.fetch(1)) }
Dir.chdir(ARGV.fetch(2)) { f.install }
"""
        result = subprocess.run(["brew", "ruby", "-e", ruby, "--", str(formula), str(prefix), str(stage), str(invalid_config)], env=env,
                                text=True, capture_output=True, timeout=90)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for rel in ["bin/bcmr", "share/zsh/site-functions/_bcmr",
                    "etc/bash_completion.d/bcmr", "share/fish/vendor_completions.d/bcmr.fish"]:
            self.assertTrue((prefix / rel).is_file(), f"missing installed file: {rel}")
            self.assertGreater((prefix / rel).stat().st_size, 0)

    def test_missing_checksum_cannot_produce_a_formula(self):
        self.manifest.write_text(f"{'a' * 64}  {ASSETS[0]}\n")
        result = self.render()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("checksum", result.stderr.lower())

    def test_invalid_version_cannot_produce_a_formula(self):
        result = self.render("v0.7.0;echo injected")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("version", result.stderr.lower())


if __name__ == "__main__":
    unittest.main()
