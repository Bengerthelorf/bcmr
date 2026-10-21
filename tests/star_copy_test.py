#!/usr/bin/env python3
"""Source-star regressions against real CLI/filesystem/shell/serve processes.

Only the external SSH boundary is replaced. Run with
BCMR=/absolute/path/to/bcmr python3 tests/star_copy_test.py.
"""
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

from shell_test_utils import with_system_zsh_fpath

BCMR = str(Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve())
ZSH = shutil.which("zsh")


@unittest.skipUnless(os.name == "posix", "requires POSIX filesystem and shell")
class SourceStarTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="bcmr-stars-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.local = self.root / "local"
        self.home = self.root / "fixture-home"
        self.second = self.root / "second-home"
        self.dest = self.root / "destination"
        self.bin = self.root / "bin"
        for path in (self.local, self.home, self.second, self.dest, self.bin):
            path.mkdir()
        (self.bin / "bcmr").symlink_to(BCMR)
        ssh = self.bin / "ssh"
        ssh.write_text('''#!/usr/bin/env python3
import os
import sys
args = sys.argv[1:]
while args and args[0] == '-o':
    args = args[2:]
if len(args) < 2:
    sys.exit('test SSH transport expected host and remote command')
host, words = args[0], args[1:]
if host == 'bad-auth':
    sys.stderr.write('Permission denied (publickey).\\n')
    sys.exit(255)
if host == 'denied-list':
    sys.stderr.write('ls: protected directory: Permission denied\\n')
    sys.exit(1)
if host not in ('fixture', 'second', 'legacy'):
    sys.exit('unexpected SSH host: ' + host)
home = os.environ['STAR_SECOND_HOME' if host == 'second' else 'STAR_FIXTURE_HOME']
if host == 'legacy' and words == ['bcmr', 'serve']:
    sys.stderr.write('bcmr: protocol version mismatch: client=2 server=1\\n')
    sys.exit(1)
os.chdir(home)
os.environ['HOME'] = home
# Model a source created while the first fan-out destination connects. This
# is a transport side effect, never an implementation of pattern matching.
late = os.environ.get('STAR_LATE_SOURCE')
if late:
    with open(late, 'w') as stream:
        stream.write('created after source expansion')
# OpenSSH joins command arguments and executes them with the login shell.
os.execv('/bin/sh', ['sh', '-c', ' '.join(words)])
''')
        ssh.chmod(0o755)
        self.env = with_system_zsh_fpath(dict(
            os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
            HOME=str(self.root), ZDOTDIR=str(self.root),
            XDG_CONFIG_HOME=str(self.root / "config"),
            XDG_CACHE_HOME=str(self.root / "cache"),
            XDG_DATA_HOME=str(self.root / "data"),
            STAR_FIXTURE_HOME=str(self.home), STAR_SECOND_HOME=str(self.second),
            BCMR_SSH_NO_MULTIPLEX="1", NO_COLOR="1", TERM="dumb"))
        self.env.pop("BCMR_CONFIG", None)

    def put(self, base, name, content=None):
        path = base / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content if content is not None else name)
        return path

    def run_bcmr(self, operation, *args, env=None):
        return subprocess.run([BCMR, operation, "--progress=off", *map(str, args)],
                              env=env or self.env, cwd=self.local,
                              text=True, capture_output=True, timeout=30)

    def assert_ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def assert_files(self, base, expected):
        actual = {str(p.relative_to(base)): p.read_text()
                  for p in base.rglob("*") if p.is_file()}
        self.assertEqual(actual, expected)

    # Fails if '*' cannot match zero characters or is left as a literal source.
    def test_local_star_matches_zero_or_more_characters(self):
        self.put(self.local, "report.txt", "empty match")
        self.put(self.local, "report-final.txt", "long match")
        self.put(self.local, "unrelated.txt", "leave behind")
        result = self.run_bcmr("copy", self.local / "report*.txt", self.dest)
        self.assert_ok(result)
        self.assert_files(self.dest, {"report.txt": "empty match", "report-final.txt": "long match"})

    # A name discovered locally must never be reinterpreted as a remote host.
    def test_relative_local_match_with_colon_keeps_local_identity(self):
        self.put(self.local, "fixture:notes.txt", "local colon name")
        self.assert_ok(self.run_bcmr("copy", "*", self.dest))
        self.assert_files(self.dest, {"fixture:notes.txt": "local colon name"})

    # Fails if a component's '*' crosses '/' or silently includes hidden names.
    def test_remote_tilde_multiple_components_and_hidden_rules(self):
        self.put(self.home, "batch-one/run-a/data-one.txt", "first")
        self.put(self.home, "batch-two/run-b/data-two.txt", "second")
        self.put(self.home, ".batch-hidden/run-c/data-hidden-parent.txt", "hidden parent")
        self.put(self.home, "batch-three/.run-hidden/data-hidden-child.txt", "hidden child")
        result = self.run_bcmr("copy", "fixture:~/*/*/data-*.txt", self.dest)
        self.assert_ok(result)
        self.assert_files(self.dest, {"data-one.txt": "first", "data-two.txt": "second"})

    # Control proves the SSH boundary really reaches bcmr serve and copies bytes.
    def test_remote_literal_source_transport_control(self):
        self.put(self.home, "literal.txt", "real remote content")
        self.assert_ok(self.run_bcmr("copy", "fixture:literal.txt", self.dest))
        self.assert_files(self.dest, {"literal.txt": "real remote content"})

    # Fails if legacy fallback sees the original pattern after serve is unavailable.
    def test_remote_star_works_when_serve_negotiation_fails(self):
        self.put(self.home, "legacy-one.txt", "legacy bytes")
        self.assert_ok(self.run_bcmr("copy", "legacy:~/legacy-*", self.dest))
        self.assert_files(self.dest, {"legacy-one.txt": "legacy bytes"})

    # Fails if a leading dot is always rejected instead of honoring explicit dot.
    def test_explicit_dot_component_includes_hidden_files(self):
        self.put(self.home, ".secret-one", "secret")
        self.put(self.home, "visible", "public")
        self.assert_ok(self.run_bcmr("copy", "fixture:~/.*", self.dest))
        self.assert_files(self.dest, {".secret-one": "secret"})

    # Fails if the host:// spelling or non-ASCII source prefix is damaged.
    def test_double_slash_remote_absolute_chinese_prefix(self):
        source = self.put(self.home, "Downloads/deecamp中期检查 一.txt", "中文内容")
        self.put(self.home, "Downloads/other.txt", "ignore")
        pattern = f"fixture:/{source.parent}/deecamp中期检查*"
        self.assert_ok(self.run_bcmr("copy", pattern, self.dest))
        self.assert_files(self.dest, {source.name: "中文内容"})

    # Fails if shell quoting or newline-delimited listing corrupts real names.
    def test_remote_matches_preserve_spaces_quotes_newlines_and_shell_syntax(self):
        names = ["match 中文.txt", "match 'quote.txt", "match\nline.txt",
                 "match $(touch INJECTED).txt", "match `touch ALSO_INJECTED`.txt"]
        for number, name in enumerate(names):
            self.put(self.home, name, str(number))
        self.assert_ok(self.run_bcmr("copy", "fixture:~/match*", self.dest))
        self.assert_files(self.dest, {name: str(number) for number, name in enumerate(names)})
        self.assertFalse((self.home / "INJECTED").exists())
        self.assertFalse((self.home / "ALSO_INJECTED").exists())

    # Fails if '?' and bracket classes acquire glob semantics alongside '*'.
    def test_only_star_has_pattern_meaning(self):
        self.put(self.local, "part?[ab]-one.txt", "literal punctuation")
        self.put(self.local, "partXa-one.txt", "would match general glob")
        self.assert_ok(self.run_bcmr("copy", self.local / "part?[ab]-*.txt", self.dest))
        self.assert_files(self.dest, {"part?[ab]-one.txt": "literal punctuation"})

    # A shell may have already selected an actual filename containing '*'.
    def test_exact_existing_asterisk_path_is_preferred_as_literal(self):
        self.put(self.local, "literal*.txt", "exact")
        self.put(self.local, "literal-other.txt", "not selected")
        self.assert_ok(self.run_bcmr("copy", self.local / "literal*.txt", self.dest))
        self.assert_files(self.dest, {"literal*.txt": "exact"})

    # Fails if expansion is applied indiscriminately to all CLI paths.
    def test_destination_asterisk_is_literal(self):
        source = self.put(self.local, "source", "content")
        destination = self.root / "new*.txt"
        self.assert_ok(self.run_bcmr("copy", source, destination))
        self.assertEqual(destination.read_text(), "content")

    # Fails if valid sources start copying before later no-match validation.
    def test_unmatched_source_prevents_all_destination_writes(self):
        source = self.put(self.local, "valid.txt", "valid")
        result = self.run_bcmr("copy", source, self.local / "absent*", self.dest)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("absent*", result.stderr)
        self.assert_files(self.dest, {})

    # Fails if SSH/listing errors are mistaken for an empty match list.
    def test_remote_auth_and_listing_failures_are_visible_before_writes(self):
        for host, diagnostic in (("bad-auth", "authentication"),
                                 ("denied-list", "Permission denied")):
            with self.subTest(host=host):
                result = self.run_bcmr("copy", f"{host}:/protected/match*", self.dest)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(host, result.stderr)
                self.assertIn(diagnostic, result.stderr)
                self.assert_files(self.dest, {})

    # Fails if a directory matched by '*' bypasses the explicit recursion gate.
    def test_matched_directory_requires_recursive(self):
        self.put(self.local, "project-one/readme.txt", "tree")
        result = self.run_bcmr("copy", self.local / "project-*", self.dest)
        self.assertNotEqual(result.returncode, 0)
        self.assertRegex(result.stderr.lower(), r"recursive|\-r")
        self.assert_files(self.dest, {})
        self.assert_ok(self.run_bcmr("copy", "-r", self.local / "project-*", self.dest))
        self.assert_files(self.dest, {"project-one/readme.txt": "tree"})

    # Fails if filesystem enumeration order leaks into the copy plan.
    def test_expanded_local_sources_are_sorted_in_dry_run(self):
        for name in ("sort-z.txt", "sort-a.txt", "sort-m.txt"):
            self.put(self.local, name)
        result = self.run_bcmr("copy", "--dry-run", self.local / "sort-*", self.dest)
        self.assert_ok(result)
        output = result.stdout + result.stderr
        positions = [output.find(str(self.local / name))
                     for name in ("sort-a.txt", "sort-m.txt", "sort-z.txt")]
        self.assertTrue(all(position >= 0 for position in positions), output)
        self.assertEqual(positions, sorted(positions), output)
        self.assert_files(self.dest, {})

    # Fails if --to repeats source expansion after the first target connects.
    def test_fan_out_uses_one_source_expansion_snapshot(self):
        self.put(self.local, "fan-old.txt", "initial source")
        (self.home / "out").mkdir()
        (self.second / "out").mkdir()
        env = dict(self.env, STAR_LATE_SOURCE=str(self.local / "fan-new.txt"))
        result = self.run_bcmr("copy", "--to", "fixture:out/", "--to", "second:out/",
                               self.local / "fan-*", env=env)
        self.assert_ok(result)
        self.assertTrue((self.local / "fan-new.txt").exists())
        self.assert_files(self.home / "out", {"fan-old.txt": "initial source"})
        self.assert_files(self.second / "out", {"fan-old.txt": "initial source"})

    # Fails if download pooling sends the second host's path to the first host.
    def test_same_source_pattern_on_two_hosts_uses_each_host(self):
        self.put(self.home, "shared/host-one.txt", "host one")
        self.put(self.second, "shared/host-two.txt", "host two")
        result = self.run_bcmr("copy", "fixture:shared/host-*", "second:shared/host-*", self.dest)
        self.assert_ok(result)
        self.assert_files(self.dest, {"host-one.txt": "host one", "host-two.txt": "host two"})

    # Fails if remote dispatch quietly drops a source from the local half.
    def test_mixed_local_and_remote_sources_rejected_before_writes(self):
        source = self.put(self.local, "local.txt", "local")
        self.put(self.home, "remote.txt", "remote")
        result = self.run_bcmr("copy", source, "fixture:remote*", self.dest)
        self.assertNotEqual(result.returncode, 0)
        self.assertRegex(result.stderr.lower(), r"mixed|mixing|local.*remote|remote.*local")
        self.assert_files(self.dest, {})

    # Fails if move deletes nonmatching sources or fails to remove copied matches.
    def test_remote_move_removes_only_matched_sources_after_success(self):
        self.put(self.home, "move-one.txt", "first")
        self.put(self.home, "move-two.txt", "second")
        self.put(self.home, "keep.txt", "keep")
        self.assert_ok(self.run_bcmr("move", "fixture:~/move-*", self.dest))
        self.assert_files(self.dest, {"move-one.txt": "first", "move-two.txt": "second"})
        self.assert_files(self.home, {"keep.txt": "keep"})

    # Fails if unsuccessful move expansion/deployment deletes selected originals.
    def test_failed_move_keeps_all_source_files(self):
        source = self.put(self.local, "move-one.txt", "preserve")
        blocker = self.put(self.root, "blocker", "not a directory")
        result = self.run_bcmr("move", self.local / "move-*", blocker / "child")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(source.read_text(), "preserve")
        self.assertEqual(blocker.read_text(), "not a directory")

    # A preview must not enter the destructive successful fan-out cleanup path.
    def test_fan_out_move_dry_run_keeps_sources_and_targets_unchanged(self):
        source = self.put(self.local, "preview-one.txt", "preserve preview source")
        other = self.root / "other-destination"
        other.mkdir()
        result = self.run_bcmr("move", "--dry-run", "preview-*", "--to", self.dest,
                               "--to", other)
        self.assert_ok(result)
        self.assertTrue(source.exists(), "dry-run deleted the source")
        self.assertEqual(source.read_text(), "preserve preview source")
        self.assert_files(self.dest, {})
        self.assert_files(other, {})

    # A source excluded from all copies must never be deleted by move cleanup.
    def test_fan_out_move_preserves_excluded_star_matches(self):
        self.put(self.local, "moving-keep.txt", "copied")
        excluded = self.put(self.local, "moving-skip.txt", "excluded")
        other = self.root / "other-destination"
        other.mkdir()
        result = self.run_bcmr("move", "moving-*", "--exclude", "skip", "--to", self.dest,
                               "--to", other)
        self.assert_ok(result)
        self.assertTrue(excluded.exists(), "excluded source was deleted")
        self.assertEqual(excluded.read_text(), "excluded")
        self.assertFalse((self.local / "moving-keep.txt").exists())
        self.assert_files(self.dest, {"moving-keep.txt": "copied"})
        self.assert_files(other, {"moving-keep.txt": "copied"})

    def test_recursive_fan_out_move_preserves_excluded_descendants(self):
        self.put(self.local, "tree-one/keep.txt", "keep")
        self.put(self.local, "tree-one/skip.txt", "excluded descendant")
        other = self.root / "other-destination"
        other.mkdir()
        result = self.run_bcmr("move", "-r", "tree-*", "--exclude", "skip", "--to", self.dest,
                               "--to", other)
        self.assert_ok(result)
        self.assert_files(self.local, {"tree-one/skip.txt": "excluded descendant"})
        self.assert_files(self.dest, {"tree-one/keep.txt": "keep"})
        self.assert_files(other, {"tree-one/keep.txt": "keep"})

    def test_remote_move_preserves_excluded_star_matches(self):
        self.put(self.home, "remote-keep.txt", "keep")
        self.put(self.home, "remote-skip.txt", "excluded")
        result = self.run_bcmr("move", "fixture:remote-*", "--exclude", "skip", self.dest)
        self.assert_ok(result)
        self.assert_files(self.home, {"remote-skip.txt": "excluded"})
        self.assert_files(self.dest, {"remote-keep.txt": "keep"})

    def test_literal_and_star_local_source_spellings_move_only_once(self):
        self.put(self.local, "unique.txt", "once")
        self.assert_ok(self.run_bcmr("move", "unique.txt", "uni*", self.dest))
        self.assert_files(self.dest, {"unique.txt": "once"})
        self.assertFalse((self.local / "unique.txt").exists())

    def test_literal_and_star_remote_source_spellings_move_only_once(self):
        self.put(self.home, "unique.txt", "once")
        self.assert_ok(self.run_bcmr("move", "fixture:unique.txt", "fixture:uni*", self.dest))
        self.assert_files(self.dest, {"unique.txt": "once"})
        self.assertFalse((self.home / "unique.txt").exists())

    def test_terminal_dot_component_cannot_match_a_regular_file(self):
        self.put(self.local, "plain-file.txt", "file")
        result = self.run_bcmr("copy", "plain*/.", self.dest)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("No matches", result.stderr)
        self.assert_files(self.dest, {})

    # Runs the generated public init API and actual unquoted commands in zsh.
    @unittest.skipUnless(ZSH, "zsh is not installed")
    def test_zsh_unquoted_bcp_bmv_stars_preserve_nomatch_and_brm(self):
        self.put(self.home, "deecamp中期检查-copy.txt", "copy")
        self.put(self.home, "deecamp中期检查-move.txt", "move")
        generated = subprocess.check_output([BCMR, "init", "zsh", "--cmd", "b"],
                                            env=self.env, text=True, timeout=15)
        script = generated + '\n' + '''
[[ -o NOMATCH ]] || exit 41
'''
        # eval parses each line after init has installed its aliases, just as
        # later interactive commands do. The source '*' is deliberately unquoted.
        for operation, suffix in (("bcp", "copy"), ("bmv", "move")):
            command = f"{operation} fixture://{str(self.home).lstrip('/')}/deecamp中期检查-{suffix}* {shlex.quote(str(self.dest))}"
            script += "eval " + shlex.quote(command) + " || exit 42\n"
        script += '''
[[ -o NOMATCH ]] || exit 43
(eval ': never-matches-987654*') 2>/dev/null && exit 44
brm_error=$(eval 'brm never-matches-987654*' 2>&1)
[[ $brm_error == *'no matches found'* ]] || exit 45
[[ -o NOMATCH ]] || exit 46
'''
        result = subprocess.run([ZSH, "-f", "-c", script], env=self.env, cwd=self.local,
                                text=True, capture_output=True, timeout=30)
        self.assert_ok(result)
        self.assert_files(self.dest, {"deecamp中期检查-copy.txt": "copy", "deecamp中期检查-move.txt": "move"})
        self.assertTrue((self.home / "deecamp中期检查-copy.txt").exists())
        self.assertFalse((self.home / "deecamp中期检查-move.txt").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
