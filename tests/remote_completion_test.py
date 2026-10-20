#!/usr/bin/env python3
"""Exercise real completion commands; replace only the external SSH transport.

Run with BCMR=/absolute/path/to/bcmr python3 tests/remote_completion_test.py.
"""

import os
from pathlib import Path
import pwd
import shutil
import subprocess
import tempfile
import unittest

from shell_test_utils import with_system_zsh_fpath


BCMR = str(Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve())


class RemoteCompletionTests(unittest.TestCase):
    def setUp(self):
        # A fixture under the account's actual home also exercises ~username
        # through the real remote shell, without faking tilde expansion.
        self.temporary = tempfile.TemporaryDirectory(prefix="bcmr-completion-", dir=Path.home())
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "remote home"
        self.home.mkdir()
        (self.home / "Documents").mkdir()
        (self.home / "Downloads").mkdir()
        (self.home / "empty").mkdir()
        (self.home / "notes.txt").touch()
        (self.home / "Documents" / "project.txt").touch()
        (self.home / "Documents" / "other.txt").touch()
        self.quoted = self.home / "space and 'quote $(touch INJECTED)"
        self.quoted.mkdir()
        (self.quoted / "a file's name.txt").touch()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "bcmr").symlink_to(BCMR)
        ssh = self.bin / "ssh"
        ssh.write_text("""#!/usr/bin/env python3
import os
import sys
args = sys.argv[1:]
while args and args[0] == '-o':
    args = args[2:]
if len(args) != 2:
    sys.exit('test SSH transport expected a target and one remote shell command')
target, command = args
if target == 'bad-auth':
    sys.stderr.write('Permission denied (publickey).\\n')
    sys.exit(255)
if target == 'denied-directory':
    if not command.startswith("LC_ALL=C ls -1ap -- "):
        sys.exit('expected a directory listing')
    sys.stderr.write('ls: /private: Permission denied\\n')
    sys.exit(1)
if target == 'unknown-host':
    sys.stderr.write('ssh: Could not resolve hostname unknown-host\\n')
    sys.exit(255)
if target not in ('fixture', 'user@fixture', '::1', 'user@::1'):
    sys.exit('unexpected SSH target: ' + target)
os.chdir(os.environ['REMOTE_HOME'])
os.environ['HOME'] = os.environ['REMOTE_HOME']
os.execv('/bin/sh', ['sh', '-c', command])
""")
        ssh.chmod(0o755)
        self.env = with_system_zsh_fpath(dict(
            os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
            REMOTE_HOME=str(self.home), BCMR_SSH_NO_MULTIPLEX="1"))
        self.env.pop("BCMR_COMPLETION_DEBUG", None)

    def complete(self, path):
        return subprocess.run([BCMR, "__complete-remote", path], env=self.env,
                              text=True, capture_output=True, timeout=15)

    def assert_candidates(self, path, expected):
        result = self.complete(path)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.stdout.splitlines(), expected)

    def test_tilde_preserves_typed_path_and_filters_prefix(self):
        self.assert_candidates("fixture:~/Do", ["fixture:~/Documents/", "fixture:~/Downloads/"])
        self.assert_candidates("fixture:~/Documents/pro", ["fixture:~/Documents/project.txt"])

    def test_bare_tilde_lists_home_children(self):
        self.assert_candidates("fixture:~", ["fixture:~/Documents/", "fixture:~/Downloads/",
                               "fixture:~/empty/", "fixture:~/notes.txt",
                               "fixture:~/space and 'quote $(touch INJECTED)/"])

    def test_named_user_tilde_uses_real_shell_home(self):
        username = pwd.getpwuid(os.getuid()).pw_name
        relative = self.home.relative_to(Path.home())
        self.assert_candidates(f"fixture:~{username}/{relative}/Do",
                               [f"fixture:~{username}/{relative}/Documents/",
                                f"fixture:~{username}/{relative}/Downloads/"])
        result = self.complete(f"fixture:~{username}")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"fixture:~{username}/{self.root.name}/", result.stdout.splitlines())

    def test_absolute_relative_and_empty_remote_paths(self):
        self.assert_candidates(f"fixture:{self.home}/Do",
                               [f"fixture:{self.home}/Documents/", f"fixture:{self.home}/Downloads/"])
        self.assert_candidates("fixture:Documents/pro", ["fixture:Documents/project.txt"])
        self.assert_candidates("user@fixture:Do", ["user@fixture:Documents/", "user@fixture:Downloads/"])
        result = self.complete("fixture:")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fixture:Documents/", result.stdout.splitlines())

    def test_spaces_quotes_and_shell_syntax_are_literal(self):
        self.assert_candidates(f"fixture:{self.quoted}/a", [f"fixture:{self.quoted}/a file's name.txt"])
        name = self.quoted.name
        self.assert_candidates(f"fixture:~/{name}/a", [f"fixture:~/{name}/a file's name.txt"])
        self.assertFalse((self.home / "INJECTED").exists())

    def test_empty_directory_and_no_matches_are_successful(self):
        self.assert_candidates(f"fixture:{self.home}/empty/", [])
        self.assert_candidates(f"fixture:{self.home}/Documents/absent", [])

    def test_missing_directory_reports_failure_with_context(self):
        result = self.complete("fixture:~/missing/")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("fixture:~/missing/", result.stderr)
        self.assertIn("No such file or directory", result.stderr)

    def test_ssh_failures_are_actionable(self):
        for host, diagnostic in [("bad-auth", "authentication failed"),
                                 ("unknown-host", "unknown host")]:
            for path in ("/tmp/", "~/"):
                with self.subTest(host=host, path=path):
                    result = self.complete(f"{host}:{path}")
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn(host, result.stderr)
                    self.assertIn(diagnostic, result.stderr)

    def test_malformed_tilde_username_cannot_execute_shell_code(self):
        result = self.complete("fixture:~$(touch INJECTED)/")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("refusing to expand", result.stderr)
        self.assertFalse((self.home / "INJECTED").exists())

    def test_unknown_tilde_user_reports_failure(self):
        result = self.complete("fixture:~bcmr_completion_nonexistent_user_987654/")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("no such user", result.stderr)

    def test_unreadable_directory_is_not_an_ssh_authentication_error(self):
        result = self.complete("denied-directory:/private/")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("Permission denied", result.stderr)
        self.assertNotIn("authentication failed", result.stderr)

    def test_macos_system_bash_does_not_require_compopt(self):
        bash = Path("/bin/bash")
        if not bash.exists():
            self.skipTest("system Bash unavailable")
        version = subprocess.check_output([str(bash), "-c", 'printf "%s" "$BASH_VERSION"'], text=True)
        if not version.startswith("3."):
            self.skipTest("this compatibility regression targets Bash 3")
        script = subprocess.check_output([BCMR, "completions", "bash"], env=self.env, text=True)
        script += '''
COMP_WORDS=(bcmr copy 'fixture:Documents/pro')
COMP_CWORD=2
_bcmr_with_remote
printf '%s\\n' "${COMPREPLY[@]}"
'''
        result = subprocess.run([str(bash), "--noprofile", "--norc"], input=script,
                                env=self.env, text=True, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "fixture:Documents/project.txt\n")
        self.assertEqual(result.stderr, "")

    def test_ipv6_candidates_keep_bracketed_host(self):
        self.assert_candidates("user@[::1]:Documents/pro", ["user@[::1]:Documents/project.txt"])

    def test_bash_completion_debug_exposes_remote_error(self):
        script = subprocess.run([BCMR, "completions", "bash"], env=self.env,
                                text=True, capture_output=True, check=True).stdout
        for debug in (False, True):
            with self.subTest(debug=debug):
                env = dict(self.env)
                if debug:
                    env["BCMR_COMPLETION_DEBUG"] = "1"
                command = script + "\nCOMP_WORDS=(bcmr copy bad-auth:/tmp/)\nCOMP_CWORD=2\n_bcmr_with_remote\n"
                result = subprocess.run(["bash", "--noprofile", "--norc", "-c", command],
                                        env=env, text=True, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stderr)
                if debug:
                    self.assertIn("authentication failed", result.stderr)
                else:
                    self.assertEqual(result.stderr, "")

    def test_bash_completion_preserves_spaces_and_quotes(self):
        script = subprocess.run([BCMR, "completions", "bash"], env=self.env,
                                text=True, capture_output=True, check=True).stdout
        env = dict(self.env, COMPLETION_PARTIAL=f"fixture:{self.quoted}/a")
        command = script + """
compopt() { :; }
COMP_WORDS=(bcmr copy "$COMPLETION_PARTIAL")
COMP_CWORD=2
_bcmr_with_remote
printf '%s\\n' "${COMPREPLY[@]}"
"""
        result = subprocess.run(["bash", "--noprofile", "--norc", "-c", command],
                                env=env, text=True, capture_output=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(result.stdout.splitlines(), [f"fixture:{self.quoted}/a file's name.txt"])

    @unittest.skipUnless(shutil.which("zsh"), "zsh is not installed")
    def test_zsh_completion_debug_exposes_remote_error(self):
        script = subprocess.run([BCMR, "completions", "zsh"], env=self.env,
                                text=True, capture_output=True, check=True).stdout
        for debug in (False, True):
            with self.subTest(debug=debug):
                env = dict(self.env)
                if debug:
                    env["BCMR_COMPLETION_DEBUG"] = "1"
                # Completion-only builtins need an interactive completion widget;
                # stub those boundaries while running the generated wrapper.
                command = "compdef() { :; }\n" + script + """
_bcmr() { :; }
words=(bcmr copy bad-auth:/tmp/)
CURRENT=3
_bcmr_with_remote
"""
                result = subprocess.run(["zsh", "-f", "-c", command], env=env,
                                        text=True, capture_output=True, timeout=15)
                self.assertEqual(result.returncode, 0, result.stderr)
                if debug:
                    self.assertIn("authentication failed", result.stderr)
                else:
                    self.assertEqual(result.stderr, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
