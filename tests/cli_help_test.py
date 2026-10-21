#!/usr/bin/env python3
"""Run behavior-bearing help examples. Set BCMR to the binary under test."""
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

from shell_test_utils import with_system_zsh_fpath

BCMR = Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve()
ZSH = shutil.which("zsh")


class HelpExampleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bcmr help ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, HOME=str(self.root), ZDOTDIR=str(self.root),
                        XDG_CONFIG_HOME=str(self.root / "config"),
                        XDG_CACHE_HOME=str(self.root / "cache"),
                        XDG_DATA_HOME=str(self.root / "data"))
        self.env.pop("BCMR_PROFILE", None)
        self.env.pop("BCMR_CONFIG", None)

    def run_bcmr(self, *args):
        return subprocess.run([str(BCMR), *args], cwd=self.root, env=self.env,
                              capture_output=True, text=True, timeout=20)

    def help_text(self, *args):
        result = self.run_bcmr(*args)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def example_args(self, line):
        # Test argument semantics locally; SSH availability is a separate concern.
        words = shlex.split(line, comments=True)
        return [word.split(":", 1)[1] if word.startswith("host:") else word
                for word in words[1:]]

    def test_directory_check_examples_compare_identical_trees(self):
        (self.root / "project").mkdir()
        (self.root / "project/payload.txt").write_text("matching content\n")
        for destination in ("archive", "archives"):
            shutil.copytree(self.root / "project", self.root / destination / "project")

        examples = [line.strip() for command in ((), ("check",))
                    for line in self.help_text(*command, "--help").splitlines()
                    if line.strip().startswith("bcmr check ") and "./project/" in line]
        self.assertTrue(examples, "directory comparison examples are missing")
        for example in examples:
            with self.subTest(example=example):
                result = self.run_bcmr(*self.example_args(example))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_compressed_directory_copy_example_copies_the_tree(self):
        (self.root / "src").mkdir()
        (self.root / "src/payload.txt").write_text("directory payload\n")
        (self.root / "dst").mkdir()
        examples = [line.strip() for line in self.help_text("copy", "--help").splitlines()
                    if line.strip().startswith("bcmr copy ") and "--compress" in line]
        self.assertTrue(examples, "wire compression example is missing")
        for example in examples:
            with self.subTest(example=example):
                result = self.run_bcmr(*self.example_args(example))
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual((self.root / "dst/src/payload.txt").read_text(),
                                 "directory payload\n")

    @unittest.skipUnless(ZSH, "zsh is required")
    def test_quick_start_init_creates_the_advertised_prefixed_commands(self):
        hint = self.help_text()
        example = next((line.strip() for line in hint.splitlines()
                        if "bcmr init zsh" in line), None)
        self.assertIsNotNone(example, "zsh initialization example is missing")
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        (bin_dir / "bcmr").symlink_to(BCMR)
        env = dict(self.env, PATH=str(bin_dir) + os.pathsep + self.env.get("PATH", ""))
        checks = "\n".join(
            f'(( $+functions[{wrapper}] )) || exit 21\n'
            f'{wrapper} --help >/dev/null || exit 22'
            for wrapper in ("bcp", "bmv", "brm"))
        result = subprocess.run([ZSH, "-f", "-c", example + "\n" + checks],
                                cwd=self.root, env=with_system_zsh_fpath(env),
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main()
