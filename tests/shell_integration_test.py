#!/usr/bin/env python3
"""Exercise generated shell scripts against a real zsh. Set BCMR to the binary."""
import os
import pathlib
import pty
import select
import signal
import shlex
import shutil
import subprocess
import tempfile
import time
import unittest

from shell_test_utils import with_system_zsh_fpath

BCMR = pathlib.Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve()
ZSH = shutil.which("zsh")


@unittest.skipUnless(ZSH, "zsh is required")
class ShellIntegrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bcmr shell ")
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.env = with_system_zsh_fpath(os.environ)
        self.env.update(HOME=str(self.root), ZDOTDIR=str(self.root), TERM="xterm",
                        XDG_DATA_HOME=str(self.root / "data with spaces"))

    def generate(self, *args):
        return subprocess.check_output([str(BCMR), "init", "zsh", *args], env=self.env, text=True)

    def run_zsh(self, script):
        result = subprocess.run([ZSH, "-f", "-c", script], env=self.env,
                                capture_output=True, text=True, timeout=20, cwd=self.root)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def assert_ready(self, script, prefix="b", suffix=""):
        checks = '\n'.join(f'[[ ${{_comps[{prefix}{cmd}{suffix}]}} == _{prefix}{cmd}{suffix} ]] || exit 21'
                           for cmd in ("cp", "mv", "rm"))
        command_checks = '\n'.join(
            f'[[ "$({prefix}{cmd}{suffix} --help)" == *"Usage: bcmr {subcommand} "* ]] || exit 23'
            for cmd, subcommand in (("cp", "copy"), ("mv", "move"), ("rm", "remove")))
        result = self.run_zsh(script + f'''
(( $+functions[compdef] && $+functions[_bcmr] && $+functions[_bcmr_with_remote] )) || exit 20
[[ ${{_comps[bcmr]}} == _bcmr_with_remote ]] || exit 22
{checks}
{command_checks}
''')
        self.assertEqual(result.stderr, "")

    # Executable names must be literal shell words, including substitutions.
    def test_executable_path_is_literal_in_generated_init(self):
        copy = self.root / 'bcmr\'s "quoted" $(touch BCMR_INJECTION) `touch BCMR_BACKTICK` 雪'
        shutil.copy2(BCMR, copy)
        script = subprocess.check_output([str(copy), "init", "zsh", "--cmd", "b"], env=self.env, text=True)
        result = self.run_zsh(script + '\nbcp --help >/dev/null || exit 30')
        self.assertEqual(result.stderr, "")
        self.assertFalse((self.root / "BCMR_INJECTION").exists())
        self.assertFalse((self.root / "BCMR_BACKTICK").exists())

    @unittest.skipUnless(shutil.which("bash"), "bash is required")
    def test_bash_executable_and_path_are_literal(self):
        copy = self.root / 'bcmr\'s "quoted" $(touch BCMR_INJECTION) `touch BCMR_BACKTICK` 雪'
        shutil.copy2(BCMR, copy)
        path = self.root / 'bin "quoted" $(touch BCMR_INJECTION) `touch BCMR_BACKTICK` 雪'
        script = subprocess.check_output([str(copy), "init", "bash", "--cmd", "b", "--path", str(path)],
                                         env=self.env, text=True)
        result = subprocess.run([shutil.which("bash"), "--noprofile", "--norc", "-c",
                                 script + '\nbcp --help >/dev/null || exit 31\nprintf \'%s\\n\' "${PATH%%:*}"'],
                                env=self.env, cwd=self.root, capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(path))
        self.assertEqual(result.stderr, "")
        self.assertFalse((self.root / "BCMR_INJECTION").exists())
        self.assertFalse((self.root / "BCMR_BACKTICK").exists())

    def test_path_argument_is_literal(self):
        path = self.root / 'bin "quoted" $(touch BCMR_INJECTION) `touch BCMR_BACKTICK` 雪'
        result = self.run_zsh(self.generate("--no-cmd", "--path", str(path)) +
                              '\nprint -r -- "$path[1]"')
        self.assertEqual(result.stdout.strip(), str(path))
        self.assertEqual(result.stderr, "")
        self.assertFalse((self.root / "BCMR_INJECTION").exists())
        self.assertFalse((self.root / "BCMR_BACKTICK").exists())

    # Removing completion bootstrapping must break a clean shell's public init API.
    def test_clean_zsh_init_loads_and_registers_completions(self):
        self.assert_ready(self.generate("--cmd", "b"))

    # An autoloaded _bcmr stub is not evidence that the remote wrapper is loaded.
    def test_initialized_zsh_autoload_stub_and_repeat_init(self):
        script = self.generate("--cmd", "b")
        self.assert_ready("autoload -Uz compinit; compinit -D\nautoload -Uz _bcmr\n" + script + script)

    def test_custom_prefix_and_suffix(self):
        self.assert_ready(self.generate("--cmd", "x", "--prefix", "p", "--suffix", "+"), "p", "+")

    def test_no_cmd_only_updates_requested_path(self):
        path = self.root / "bin with spaces"
        result = self.run_zsh(self.generate("--no-cmd", "--path", str(path)) +
                              '\n(( ! $+functions[bcp] && ! $+functions[compdef] )) || exit 24\nprint -r -- "$path[1]"')
        self.assertEqual(result.stdout.strip(), str(path))
        self.assertEqual(result.stderr, "")

    # Missing compinit must leave commands working with an actionable warning.
    def test_missing_compinit_preserves_command_wrappers(self):
        result = self.run_zsh("fpath=()\n" + self.generate("--cmd", "b") +
                              '\nbcp --help >/dev/null || exit 25')
        self.assertIn("bcmr:", result.stderr)
        self.assertIn("compinit", result.stderr)
        self.assertNotIn("command not found: compdef", result.stderr)

    # An insecure completion directory must never be silently trusted.
    def test_insecure_fpath_keeps_compinit_security_checks(self):
        insecure = self.root / "insecure"
        insecure.mkdir(mode=0o777)
        insecure.chmod(0o777)
        result = self.run_zsh("fpath=(" + shlex.quote(str(insecure)) + " $fpath)\n" +
                              self.generate("--cmd", "b") +
                              '\nbcp --help >/dev/null || exit 27\n(( ! $+functions[_bcp] )) || exit 28')
        self.assertIn("bcmr:", result.stderr)
        self.assertIn("compinit", result.stderr)

    # Reinitialization must not regenerate already loaded completion functions.
    def test_repeat_init_does_not_require_completion_binary_again(self):
        copy = self.root / "temporary bcmr"
        shutil.copy2(BCMR, copy)
        script = subprocess.check_output([str(copy), "init", "zsh", "--cmd", "b"], env=self.env, text=True)
        result = self.run_zsh(script + "\ncommand rm " + shlex.quote(str(copy)) + "\n" + script +
                              '\n(( $+functions[_bcmr_with_remote] && $+functions[_bcp] )) || exit 29')
        self.assertEqual(result.stderr, "")

    def test_completion_generation_failure_preserves_command_wrappers(self):
        copy = self.root / "temporary bcmr"
        shutil.copy2(BCMR, copy)
        script = subprocess.check_output([str(copy), "init", "zsh", "--cmd", "b"], env=self.env, text=True)
        copy.unlink()
        result = self.run_zsh(script + '\n(( $+functions[bcp] )) || exit 26')
        self.assertIn("bcmr:", result.stderr)
        self.assertIn("completion", result.stderr.lower())

    # Installation must report the actual directory and distinguish activation.
    def test_install_with_xdg_path_spaces(self):
        result = subprocess.run([str(BCMR), "completions", "zsh", "--install"],
                                env=self.env, capture_output=True, text=True, check=True)
        installed = self.root / "data with spaces/zsh/site-functions/_bcmr"
        self.assertTrue(installed.is_file())
        self.assertIn(str(installed.parent), result.stderr)
        self.assertNotIn("$XDG_DATA_HOME", result.stderr)
        self.assertIn("bcmr init zsh --cmd b", result.stderr)
        self.assertIn("activat", result.stderr.lower())
        self.assert_ready("fpath=(" + shlex.quote(str(installed.parent)) + " $fpath)\nautoload -Uz compinit; compinit -D\n" + self.generate("--cmd", "b"))
        printed = subprocess.check_output([str(BCMR), "completions", "zsh", "--install", "--print"],
                                          env=self.env, text=True)
        self.assertEqual(printed.strip(), str(installed))

    def test_install_without_xdg_reports_resolved_default(self):
        self.env.pop("XDG_DATA_HOME", None)
        installed = self.root / ".local/share/zsh/site-functions/_bcmr"
        printed = subprocess.check_output([str(BCMR), "completions", "zsh", "--install", "--print"],
                                          env=self.env, text=True)
        self.assertEqual(printed.strip(), str(installed))
        result = subprocess.run([str(BCMR), "completions", "zsh", "--install"],
                                env=self.env, capture_output=True, text=True, check=True)
        self.assertTrue(installed.is_file())
        self.assertIn(str(installed.parent), result.stderr)
        self.assertNotIn("$XDG_DATA_HOME", result.stderr)

    # A real ZLE Tab verifies that registration reaches clap flag completion.
    def test_tab_expands_alias_flag_in_pty(self):
        self.assert_tab_buffer(self.generate("--cmd", "b"), "bcp --rec", "bcp --recursive ")

    # Old sourced completion functions must be replaced after a binary upgrade.
    def test_stale_preloaded_completions_are_refreshed_in_pty(self):
        for version in (None, "0.6.3"):
            with self.subTest(version=version):
                stale = """
autoload -Uz compinit
compinit -D
_bcmr() { _arguments '--recursive[Recursively process directories]' }
_bcmr_with_remote() { _bcmr "$@" }
compdef _bcmr_with_remote bcmr
"""
                if version is not None:
                    stale += "typeset -g _bcmr_completion_version=" + shlex.quote(version) + "\n"
                self.assert_tab_buffer(stale + self.generate("--cmd", "b"),
                                       "bcp --no-de", "bcp --no-deref ")

    # Re-sourcing an old file can replace functions while leaving our version marker.
    def test_stale_completion_source_after_init_is_refreshed_in_pty(self):
        stale_file = self.root / "stale-completions.zsh"
        stale_file.write_text('''
_bcmr() { _arguments '--recursive[Recursively process directories]' }
_bcmr_with_remote() { _bcmr "$@" }
compdef _bcmr_with_remote bcmr
''')
        script = self.generate("--cmd", "b")
        self.assert_tab_buffer(script + "\nsource " + shlex.quote(str(stale_file)) + "\n" + script,
                               "bcp --no-de", "bcp --no-deref ")

    def assert_tab_buffer(self, script, typed, expected):
        init_file = self.root / "init.zsh"
        init_file.write_text(script)
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(ZSH, [ZSH, "-f", "-i"], self.env)
        output = bytearray()
        def until(marker):
            deadline = time.monotonic() + 20
            while marker not in output:
                remaining = deadline - time.monotonic()
                self.assertGreater(remaining, 0, bytes(output).decode(errors="replace"))
                if select.select([fd], [], [], remaining)[0]:
                    chunk = os.read(fd, 65536)
                    self.assertTrue(chunk, bytes(output).decode(errors="replace"))
                    output.extend(chunk)
        try:
            command = ("PS1='BCMR_PROMPT> '; source " + shlex.quote(str(init_file)) +
                       "; _bcmr_test_buffer() { print -r -- \"BCMR_BUFFER=$BUFFER:BCMR_END\"; zle send-break }; "
                       "zle -N _bcmr_test_buffer; bindkey '^X' _bcmr_test_buffer; print BCMR_READY\n")
            os.write(fd, command.encode())
            until(b"\r\nBCMR_READY\r\n")
            output.clear()
            os.write(fd, typed.encode() + b"\t\x18")
            until(b":BCMR_END\r\n")
            self.assertIn(("BCMR_BUFFER=" + expected + ":BCMR_END").encode(), output)
            self.assertNotIn(b"command not found", output)
        finally:
            os.kill(pid, signal.SIGKILL)
            os.close(fd)
            os.waitpid(pid, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
