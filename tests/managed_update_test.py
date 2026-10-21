#!/usr/bin/env python3
"""A package-manager installation must be updated through that manager."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

BCMR = Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve()


@unittest.skipIf(os.name == "nt", "Homebrew uses POSIX installation paths")
class ManagedUpdateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bcmr managed update ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        keg = self.root / "Cellar/bcmr/0.7.0"
        self.binary = keg / "bin/bcmr"
        self.binary.parent.mkdir(parents=True)
        shutil.copy2(BCMR, self.binary)
        self.receipt = keg / "INSTALL_RECEIPT.json"
        self.receipt.write_text(json.dumps({"source": {
            "tap": "bengerthelorf/tap", "spec": "stable",
            "path": str(self.root / "Library/Taps/bengerthelorf/homebrew-tap/Formula/bcmr.rb")}}))
        self.env = dict(os.environ)
        # A guarded update needs no release-server connection. Let a regression
        # fail immediately at the real network boundary rather than downloading.
        self.env.update(HTTPS_PROXY="http://127.0.0.1:9", HTTP_PROXY="http://127.0.0.1:9",
                        ALL_PROXY="http://127.0.0.1:9", NO_PROXY="")
        self.env["BCMR_CONFIG"] = str(self.root / "empty.toml")
        Path(self.env["BCMR_CONFIG"]).write_text("")

    def invoke(self, *args):
        return subprocess.run([str(self.binary), *args], env=self.env,
                              capture_output=True, text=True, timeout=15)

    def test_managed_update_preserves_binary_and_receipt_and_explains_brew(self):
        before = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        receipt = self.receipt.read_bytes()
        result = self.invoke("update")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Homebrew", result.stderr)
        self.assertIn("brew upgrade bengerthelorf/tap/bcmr", result.stderr)
        self.assertEqual(hashlib.sha256(self.binary.read_bytes()).hexdigest(), before)
        self.assertEqual(self.receipt.read_bytes(), receipt)

    def test_managed_update_reports_structured_error(self):
        result = self.invoke("update", "--json")
        self.assertEqual(result.returncode, 2)
        self.assertTrue(result.stdout.lstrip().startswith("{"), result.stdout)
        error = json.loads(result.stdout)
        self.assertIn("Homebrew", error["error"])
        self.assertIn("brew upgrade", error["error"])

    def test_standalone_update_failure_keeps_stdout_valid_json(self):
        self.receipt.unlink()
        result = self.invoke("update", "--json")
        self.assertEqual(result.returncode, 2)
        self.assertTrue(result.stdout.lstrip().startswith("{"), result.stdout)
        self.assertEqual(json.loads(result.stdout)["status"], "error")

    def test_check_is_still_a_release_check(self):
        result = self.invoke("update", "--check")
        self.assertNotEqual(result.returncode, 0)  # deliberately unreachable network
        self.assertNotIn("Homebrew manages", result.stderr)

    def test_cellar_name_alone_does_not_mark_a_standalone_binary_managed(self):
        self.receipt.unlink()
        result = self.invoke("update")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("Homebrew manages", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
