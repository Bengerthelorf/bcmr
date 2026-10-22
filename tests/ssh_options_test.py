#!/usr/bin/env python3
"""Connection overrides and scp URI routing at the real CLI boundary.

Run with BCMR=/absolute/path/to/bcmr python3 tests/ssh_options_test.py.
Only the external SSH executable is replaced; its shell commands operate on
isolated files, and every attempted invocation is captured before execution.
"""

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import unittest
from urllib.parse import quote


BCMR = str(Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve())


class SshOptionsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="bcmr-ssh-options-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "bcmr").symlink_to(BCMR)
        self.home = self.root / "home"
        self.home.mkdir()
        self.log = self.root / "ssh.jsonl"
        self.remote = self.root / "remote"
        self.remote.mkdir()
        (self.remote / "plain.txt").write_text("connection policy payload\n")
        (self.remote / "雪 #100%.txt").write_text("unicode URI payload\n")
        fake = self.bin / "ssh"
        fake.write_text("#!" + sys.executable + "\n" + r'''
import json, os, sys
args = sys.argv[1:]
with open(os.environ['SSH_LOG'], 'a') as log:
    log.write(json.dumps(args) + '\n')
while args:
    if args[0] in ('-o', '-p', '-i', '-F', '-J', '-l', '-S'):
        args = args[2:]
    elif args[0] in ('-T', '-t', '-s', '-G'):
        args = args[1:]
    elif args[0] == '--':
        args = args[1:]
        break
    else:
        break
if not args:
    sys.exit('missing SSH destination')
target, *command = args
if target not in ('fixture', 'user@fixture', '::1', 'user@::1'):
    sys.exit('unexpected SSH destination: ' + target)
if command[:2] == ['bcmr', 'serve']:
    sys.stderr.write('bcmr: command not found\n')
    sys.exit(127)
if not command:
    print('proxyjump none')
    sys.exit(0)
os.chdir(os.environ['REMOTE_HOME'])
os.execv('/bin/sh', ['sh', '-c', ' '.join(command)])
''')
        fake.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        HOME=str(self.home), XDG_CONFIG_HOME=str(self.home / ".config"),
                        XDG_STATE_HOME=str(self.home / ".state"),
                        XDG_CACHE_HOME=str(self.home / ".cache"), SSH_LOG=str(self.log),
                        REMOTE_HOME=str(self.remote))
        self.env.pop("BCMR_CONFIG", None)
        self.env.pop("BCMR_PROFILE", None)
        self.options = ["--ssh-port", "2244", "--ssh-identity", str(self.root / "key one"),
                        "--ssh-identity", str(self.root / "key two"),
                        "--ssh-config", str(self.root / "custom config"),
                        "--ssh-jump", "jumper@jump:2299"]

    def run_bcmr(self, *args):
        return subprocess.run([BCMR, *args], env=self.env, cwd=self.root,
                              text=True, capture_output=True, timeout=25)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def assert_policy(self, calls, port="2244"):
        self.assertTrue(calls, "the real command did not invoke SSH")
        for args in calls:
            for flag, value in [("-p", port), ("-i", str(self.root / "key one")),
                                ("-i", str(self.root / "key two")),
                                ("-F", str(self.root / "custom config")),
                                ("-J", "jumper@jump:2299")]:
                self.assertIn([flag, value], [args[i:i + 2] for i in range(len(args) - 1)], args)
            self.assertEqual(args.count("-p"), 1, args)

    def test_temporary_flags_apply_to_completion_and_keep_repeated_identities(self):
        result = self.run_bcmr(*self.options, "__complete-remote", f"fixture:{self.remote}/pl")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [f"fixture:{self.remote}/plain.txt"])
        self.assert_policy(self.calls())

    def test_generated_bash_completion_replays_options_around_subcommand(self):
        generated = self.run_bcmr("completions", "bash")
        self.assertEqual(generated.returncode, 0, generated.stderr)
        partial = f"fixture:{self.remote}/pl"
        for words in [["bcmr", *self.options, "copy", partial],
                      ["bcmr", "copy", *self.options, partial]]:
            with self.subTest(words=words):
                self.log.unlink(missing_ok=True)
                script = generated.stdout + "\nCOMP_WORDS=(" + shlex.join(words) + ")\n"
                script += "COMP_CWORD=$((${#COMP_WORDS[@]} - 1))\n_bcmr_with_remote\n"
                script += "printf '%s\\n' \"${COMPREPLY[@]}\"\n"
                result = subprocess.run(["bash", "--noprofile", "--norc", "-c", script],
                                        env=self.env, cwd=self.root, text=True,
                                        capture_output=True, timeout=25)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), [f"fixture:{self.remote}/plain.txt"])
                self.assert_policy(self.calls())

    def test_uri_decodes_absolute_utf8_path_and_inline_port_overrides_global(self):
        uri = f"scp://user@fixture:2255{quote(str(self.remote / '雪 #100%.txt'))}"
        result = self.run_bcmr(*self.options, "copy", "--progress=off", "--direct=ssh", uri, "copied")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "copied").read_text(), "unicode URI payload\n")
        self.assert_policy(self.calls(), "2255")

    def test_ipv6_uri_is_decomposed_without_losing_port(self):
        uri = f"scp://user@[::1]:2256{quote(str(self.remote / 'plain.txt'))}"
        result = self.run_bcmr(*self.options, "copy", "--progress=off", uri, "copied")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "copied").read_text(), "connection policy payload\n")
        self.assert_policy(self.calls(), "2256")

    def test_uri_source_star_roundtrips_percent_characters_and_port(self):
        uri = f"scp://fixture:2257{quote(str(self.remote))}/*100%25.txt"
        result = self.run_bcmr(*self.options, "copy", "--progress=off", uri, "copied")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "copied").read_text(), "unicode URI payload\n")
        self.assert_policy(self.calls(), "2257")

    def test_same_host_sources_keep_distinct_endpoint_ports(self):
        destination = self.root / "downloads"
        uris = [f"scp://fixture:2290{quote(str(self.remote / 'plain.txt'))}",
                f"scp://fixture:2291{quote(str(self.remote / '雪 #100%.txt'))}"]
        result = self.run_bcmr(*self.options, "copy", "--progress=off", *uris, str(destination) + "/")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((destination / "plain.txt").read_text(), "connection policy payload\n")
        self.assertEqual((destination / "雪 #100%.txt").read_text(), "unicode URI payload\n")
        ports = set()
        for args in self.calls():
            self.assertEqual(args.count("-p"), 1, args)
            ports.add(args[args.index("-p") + 1])
        self.assertEqual(ports, {"2290", "2291"})

    def test_explicit_invalid_uri_is_rejected_before_source_pattern_io(self):
        for uri in ["scp://fixture:0/dst", "scp://fixture:65536/dst", "scp://fixture:abc/dst",
                    "scp://[::1/dst", "scp://fixture/%GG", "scp://fixture/%FF",
                    "scp://fixture/%00", "scp://fixture/path?query", "scp://fixture/path#fragment",
                    "scp:///dst", "scp://-oProxyCommand=bad/dst"]:
            with self.subTest(uri=uri):
                self.log.unlink(missing_ok=True)
                result = self.run_bcmr("copy", f"fixture:{self.remote}/*.txt", uri)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("scp URI", result.stderr)
                self.assertEqual(self.calls(), [], result.stderr)
                self.assertFalse((self.root / "scp:").exists())

    def test_uri_completion_retains_escaped_names_and_port(self):
        uri = f"scp://fixture:2258{quote(str(self.remote))}/%E9%9B%AA"
        result = self.run_bcmr(*self.options, "__complete-remote", uri)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(),
                         [f"scp://fixture:2258{quote(str(self.remote / '雪 #100%.txt'))}"])
        self.assert_policy(self.calls(), "2258")

    def test_doctor_and_deploy_share_temporary_policy(self):
        self.run_bcmr(*self.options, "doctor", "fixture", "--json")
        self.assert_policy(self.calls())
        self.log.unlink()
        result = self.run_bcmr(*self.options, "deploy", "fixture", "--path", BCMR)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_policy(self.calls())

    def test_background_worker_replays_connection_flags(self):
        result = self.run_bcmr(*self.options, "copy", "--background", "--json",
                               f"fixture:{self.remote}/plain.txt", "background-copy")
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads(result.stdout)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if (self.root / "background-copy").exists():
                break
            time.sleep(0.05)
        self.assertTrue((self.root / "background-copy").exists(), Path(receipt["log"]).read_text())
        self.assertEqual((self.root / "background-copy").read_text(), "connection policy payload\n")
        self.assert_policy(self.calls())

    def test_mux_identity_changes_for_each_temporary_connection_override(self):
        paths = []
        self.env.pop("BCMR_SSH_NO_MULTIPLEX", None)
        for options in [[], ["--ssh-port", "2298"], ["--ssh-port", "2299"],
                        ["--ssh-identity", str(self.root / "identity")],
                        ["--ssh-config", str(self.root / "config")], ["--ssh-jump", "jump"]]:
            self.log.unlink(missing_ok=True)
            result = self.run_bcmr(*options, "__complete-remote", f"fixture:{self.remote}/pl")
            self.assertEqual(result.returncode, 0, result.stderr)
            args = self.calls()[0]
            controls = [a for a in args if a.startswith("ControlPath=")]
            self.assertEqual(len(controls), 1, args)
            paths.append(controls[0])
        if paths[0] != "ControlPath=none":
            self.assertEqual(len(set(paths)), len(paths), paths)

    def test_legacy_worker_and_config_flags_keep_their_meaning(self):
        config = self.root / "bcmr.toml"
        config.write_text("[transfer]\nfallback_warning = false\n")
        result = self.run_bcmr("--config", str(config), "copy", "-P", "2", "--progress=off",
                               f"fixture:{self.remote}/plain.txt", "copied")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "copied").read_text(), "connection policy payload\n")
        self.assertTrue(all("-F" not in args and "-p" not in args for args in self.calls()))


if __name__ == "__main__":
    unittest.main()
