#!/usr/bin/env python3
"""Deploy behavior through real remote shells and isolated transport doubles.

Run with BCMR=/absolute/path/to/bcmr PYTHONDONTWRITEBYTECODE=1 python3 tests/deploy_test.py.
The doubles replace SSH/SFTP/HTTP only. Remote shell quoting, executable versions,
tar extraction, chmod and sudo's install command all execute normally, without
network access, passwords, root privileges or changes outside the fixture.
"""

import errno
import io
import json
import os
from pathlib import Path
import platform
import select
import shlex
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest


BCMR = Path(os.environ.get("BCMR", "target/debug/bcmr")).resolve()


# One executable is exposed under several names. SSH executes the submitted
# command verbatim in /bin/sh; it does not recognize production script text.
TRANSPORT = r'''#!PYTHON
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys

tool = Path(sys.argv[0]).name
args = sys.argv[1:]
config = json.loads(Path(os.environ['DEPLOY_FIXTURE_CONFIG']).read_text())
def record(**fields):
    with open(config['log'], 'a') as stream:
        stream.write(json.dumps(dict(tool=tool, args=args, **fields)) + '\n')

if tool in ('ssh', 'sftp'):
    rest = list(args)
    while rest and rest[0].startswith('-'):
        option = rest.pop(0)
        if option == '--':
            break
        if option in ('-o', '-b', '-p', '-P', '-l', '-i', '-F'):
            if not rest:
                sys.exit('missing SSH option argument')
            rest.pop(0)
        elif option not in ('-t', '-tt', '-T', '-q'):
            record(rejected_option=option)
            sys.exit('unsafe or unexpected SSH option: ' + option)
    if not rest:
        record(error='missing host')
        sys.exit('missing host')
    host = rest.pop(0)
    record(host=host, stdin_tty=os.isatty(0), stderr_tty=os.isatty(2))
    if host not in config['hosts']:
        sys.exit('unknown fixture host: ' + host)
    details = config['hosts'][host]
    if details.get('unreachable'):
        sys.exit('fixture connection refused')
    home = Path(details['home'])
    if tool == 'sftp':
        for line in sys.stdin:
            words = shlex.split(line)
            if not words:
                continue
            if len(words) != 3 or words[0] != 'put':
                sys.exit('fixture SFTP supports put SOURCE DESTINATION')
            source, destination = words[1:]
            destination = Path(destination)
            if not destination.is_absolute():
                destination = home / destination
            record(host=host, destination=str(destination))
            if details.get('sftp_failure'):
                destination.write_bytes(b'fixture partial transfer\n')
                sys.exit('fixture SFTP transfer failed after writing partial data')
            # A broken remote executable must be detected by post-install
            # verification, even when the file transfer reports success.
            source = details.get('replacement_upload', source)
            shutil.copyfile(source, destination)
        sys.exit(0)
    os.chdir(home)
    os.environ.update(HOME=str(home), TMPDIR=str(home / 'tmp'), DEPLOY_FIXTURE_HOST=host,
                      PATH=details['path'])
    if any(arg in ('-t', '-tt', 'RequestTTY=force', 'RequestTTY=yes') for arg in args):
        # OpenSSH forwards the remote PTY's merged output through its local
        # stdout, including sudo prompts originally written to remote stderr.
        os.dup2(1, 2)
    os.execv('/bin/sh', ['sh', '-c', ' '.join(rest)])

host = os.environ.get('DEPLOY_FIXTURE_HOST')
details = config['hosts'].get(host, {})
record(host=host, stdin_tty=os.isatty(0), stderr_tty=os.isatty(2))
if tool == 'uname':
    print(details['uname'])
elif tool == 'curl':
    # The actual tar extractor consumes an actual release archive. No GitHub
    # connection or production download implementation is involved here.
    if host and details.get('http_failure'):
        sys.exit('fixture remote HTTP download failed')
    archive = Path(config['archive'])
    if '-o' in args:
        shutil.copyfile(archive, args[args.index('-o') + 1])
    else:
        sys.stdout.buffer.write(archive.read_bytes())
elif tool == 'sudo':
    if any(arg in ('-S', '--stdin') for arg in args):
        sys.exit('fixture refuses application password capture')
    mode = details.get('sudo', 'allowed')
    if mode == 'denied' or (mode == 'tty' and ('-n' in args or not os.isatty(0))):
        sys.exit('sudo: a password is required (fixture; no password is read)')
    if mode == 'tty' and '-n' not in args:
        sys.stderr.write('FIXTURE_SUDO_PROMPT (no password or input is read)\n')
        sys.stderr.flush()
    command = list(args)
    while command and command[0] in ('-n', '--'):
        command.pop(0)
    if command == ['-v']:
        sys.exit(0)
    if not command:
        sys.exit('missing sudo command')
    os.execvp(command[0], command)
else:
    sys.exit('unexpected fixture tool: ' + tool)
'''


@unittest.skipIf(os.name == "nt", "deploy fixtures require POSIX shells and PTYs")
class DeployTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        result = subprocess.run([str(BCMR), "--version"], capture_output=True,
                                text=True, timeout=10, check=True)
        cls.version = result.stdout.strip().split()[-1]

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="bcmr deploy ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "transport"
        self.bin.mkdir()
        driver = self.bin / "driver"
        driver.write_text(TRANSPORT.replace("#!PYTHON", "#!" + sys.executable, 1))
        driver.chmod(0o755)
        for command in ("ssh", "sftp", "curl", "uname", "sudo"):
            (self.bin / command).symlink_to(driver)
        archive = self.root / "release.tar.gz"
        executable = self.version_script(self.version).encode()
        with tarfile.open(archive, "w:gz") as stream:
            member = tarfile.TarInfo("bcmr")
            member.size = len(executable)
            member.mode = 0o755
            stream.addfile(member, io.BytesIO(executable))
        self.log = self.root / "calls.jsonl"
        self.config_path = self.root / "fixture.json"
        self.config = {"hosts": {}, "log": str(self.log), "archive": str(archive)}
        config = self.root / "empty.toml"
        config.write_text("")
        self.local_tmp = self.root / "local-tmp"
        self.local_tmp.mkdir()
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        BCMR_CONFIG=str(config), DEPLOY_FIXTURE_CONFIG=str(self.config_path),
                        PYTHONDONTWRITEBYTECODE="1", BCMR_SSH_NO_MULTIPLEX="1",
                        NO_COLOR="1", TERM="dumb", TMPDIR=str(self.local_tmp))
        self.local_uname = platform.system() + " " + platform.machine()

    @staticmethod
    def version_script(version):
        return "#!/bin/sh\nprintf '%s\\n' " + shlex.quote("bcmr " + version) + "\n"

    def put_version(self, path, version):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.version_script(version))
        path.chmod(0o755)
        return path

    def host(self, name, *, selected=None, path_version=None, uname=None, sudo="allowed"):
        home = self.root / name
        home.mkdir()
        (home / "tmp").mkdir()
        path_bin = home / "path-bin"
        path_bin.mkdir()
        if path_version is not None:
            self.put_version(path_bin / "bcmr", path_version)
        self.config["hosts"][name] = {
            "home": str(home), "uname": uname or self.local_uname, "sudo": sudo,
            "path": os.pathsep.join((str(path_bin), str(self.bin), "/usr/bin", "/bin")),
        }
        if selected is not None:
            self.put_version(home / ".local/bin/bcmr", selected)
        return home

    def calls(self, tool=None, host=None):
        rows = [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []
        return [row for row in rows if (tool is None or row["tool"] == tool)
                and (host is None or row.get("host") == host)]

    def invoke(self, *args, interactive=False, stdout_file=None):
        self.config_path.write_text(json.dumps(self.config))
        command = [str(BCMR), "deploy", *map(str, args)]
        if not interactive:
            return subprocess.run(command, env=self.env, text=True,
                                  capture_output=True, timeout=25)
        import fcntl
        import pty
        import termios
        master, slave = pty.openpty()
        def terminal_session():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        redirected_stdout = stdout_file.open("wb") if stdout_file is not None else None
        try:
            process = subprocess.Popen(command, env=self.env, stdin=slave,
                                       stdout=redirected_stdout if redirected_stdout is not None else slave,
                                       stderr=slave, preexec_fn=terminal_session)
        finally:
            if redirected_stdout is not None:
                redirected_stdout.close()
        os.close(slave)
        output = bytearray()
        deadline = time.monotonic() + 25
        try:
            while True:
                if time.monotonic() > deadline:
                    os.killpg(process.pid, signal.SIGKILL)
                    self.fail("interactive deploy waited for input or hung")
                ready, _, _ = select.select([master], [], [], 0.1)
                if ready:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        break
                    if not data:
                        break
                    output.extend(data)
                elif process.poll() is not None:
                    break
            return subprocess.CompletedProcess(command, process.wait(timeout=5), "",
                                               output.decode(errors="replace"))
        finally:
            os.close(master)
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()

    def assert_version(self, path, version=None):
        self.assertTrue(path.is_file(), "expected remote executable: " + str(path))
        result = subprocess.run([str(path), "--version"], capture_output=True,
                                text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "bcmr " + (version or self.version))

    def assert_summary(self, result, installed=0, skipped=0, failed=0):
        output = result.stdout + result.stderr
        # Counts may precede or follow the labels. Avoid pinning punctuation or
        # a whole human-facing sentence while still requiring all three counts.
        for label, count in (("installed", installed), ("skipped", skipped), ("failed", failed)):
            expression = rf"(?:\b{count}\s+{label}\b|\b{label}\s*[:=]?\s*{count}\b)"
            self.assertRegex(output.lower(), expression)

    def assert_selected_and_path_reported(self, result, selected, path, path_version):
        output = result.stdout + result.stderr
        self.assertIn(str(selected), output)
        self.assertIn(str(path), output)
        self.assertIn("bcmr " + self.version, output)
        self.assertIn("bcmr " + path_version, output)
        self.assertRegex(output.lower(), r"shadow|resolves to|different.*path|path.*different")

    def test_explicit_selected_old_binary_updates_even_when_path_is_current(self):
        home = self.host("explicit", path_version=self.version)
        selected = self.put_version(home / "chosen/bin/bcmr", "0.1.0")
        result = self.invoke("explicit", "--path", selected)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(selected)
        self.assert_selected_and_path_reported(result, selected, home / "path-bin/bcmr", self.version)
        self.assert_summary(result, installed=1)

    def test_default_selected_old_binary_updates_even_when_path_is_current(self):
        home = self.host("default", selected="0.1.0", path_version=self.version)
        result = self.invoke("default")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr")
        self.assert_summary(result, installed=1)

    def test_version_prefix_is_not_treated_as_the_current_version(self):
        home = self.host("prefix", selected=self.version + "1", path_version=self.version + "1")
        result = self.invoke("prefix")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr")
        self.assert_summary(result, installed=1)

    def test_current_version_stdout_with_failed_exit_does_not_skip_installation(self):
        home = self.host("broken-version", selected=self.version)
        selected = home / ".local/bin/bcmr"
        selected.write_text(self.version_script(self.version) + "exit 42\n")
        result = self.invoke("broken-version")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(selected)
        self.assertTrue(self.calls("sftp", "broken-version"),
                        "a failing selected executable must be replaced even when it prints the expected version")
        self.assert_summary(result, installed=1)

    def test_current_selected_binary_skips_and_reports_old_path_shadow(self):
        home = self.host("skip", selected=self.version, path_version="0.1.0")
        selected = home / ".local/bin/bcmr"
        before = selected.read_bytes()
        result = self.invoke("skip")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(selected.read_bytes() == before, "a current selected executable must be skipped")
        self.assertEqual(self.calls("sftp"), [], "a skipped install must not upload")
        self.assert_selected_and_path_reported(result, selected, home / "path-bin/bcmr", "0.1.0")
        self.assert_summary(result, skipped=1)

    def test_default_install_reports_path_absence(self):
        home = self.host("fresh")
        result = self.invoke("fresh")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr")
        self.assertIn(str(home / ".local/bin/bcmr"), result.stderr)
        self.assertRegex(result.stderr.lower(), r"not.*(?:on.*path|found)|path.*(?:missing|absent|none)")
        self.assert_summary(result, installed=1)

    def test_post_install_wrong_version_is_a_failure(self):
        home = self.host("wrong")
        replacement = self.put_version(self.root / "wrong-upload", "99.99.99")
        self.config["hosts"]["wrong"]["replacement_upload"] = str(replacement)
        result = self.invoke("wrong")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr", "99.99.99")
        self.assertIn("99.99.99", result.stderr)
        self.assertIn(self.version, result.stderr)
        self.assertIn("wrong", result.stderr)
        self.assert_summary(result, failed=1)

    def test_post_install_current_version_stdout_with_failed_exit_is_a_failure(self):
        home = self.host("broken-upload")
        replacement = self.put_version(self.root / "broken-version-upload", self.version)
        replacement.write_text(self.version_script(self.version) + "exit 42\n")
        self.config["hosts"]["broken-upload"]["replacement_upload"] = str(replacement)
        result = self.invoke("broken-upload")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        installed = subprocess.run([str(home / ".local/bin/bcmr"), "--version"],
                                   capture_output=True, text=True, timeout=10)
        self.assertEqual(installed.stdout.strip(), "bcmr " + self.version)
        self.assertEqual(installed.returncode, 42)
        self.assert_summary(result, failed=1)

    def test_hosts_are_independent_and_continue_after_platform_failure(self):
        first = self.host("first")
        broken = self.host("unsupported", uname="Plan9 mystery")
        same = self.host("same", selected=self.version, path_version="0.1.0")
        cross_uname = "Linux x86_64" if platform.system() != "Linux" else "Darwin arm64"
        last = self.host("last", uname=cross_uname)
        result = self.invoke("first", "unsupported", "same", "last")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assert_version(first / ".local/bin/bcmr")
        self.assertFalse((broken / ".local/bin/bcmr").exists())
        self.assert_version(same / ".local/bin/bcmr")
        self.assert_version(last / ".local/bin/bcmr")
        self.assertEqual(self.calls("sftp", "same"), [])
        for host in ("first", "unsupported", "last"):
            self.assertTrue(self.calls("uname", host), "system/architecture must be checked for " + host)
            self.assertIn(host, result.stderr)
        self.assertTrue(self.calls("curl"), "the cross-platform host must use its own release asset")
        asset = "bcmr-x86_64-linux.tar.gz" if cross_uname.startswith("Linux") else "bcmr-aarch64-macos.tar.gz"
        self.assertTrue(any(asset in " ".join(row["args"]) for row in self.calls("curl")),
                        "the release asset must match this host's OS and architecture")
        self.assert_summary(result, installed=2, skipped=1, failed=1)

    def test_failed_upload_preserves_installed_binary_cleans_stage_and_continues(self):
        partial = self.host("partial", selected="0.1.0")
        selected = partial / ".local/bin/bcmr"
        before = selected.read_bytes()
        later = self.host("after-upload-failure")
        self.config["hosts"]["partial"]["sftp_failure"] = True
        result = self.invoke("partial", "after-upload-failure")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        uploads = [row["destination"] for row in self.calls("sftp", "partial")
                   if "destination" in row]
        self.assertTrue(uploads, "the failing host must reach its file transfer")
        self.assertTrue(selected.read_bytes() == before, "a failed transfer must preserve the old installation")
        self.assert_version(selected, "0.1.0")
        for stage in uploads:
            self.assertNotEqual(Path(stage), selected, "uploads must use a private stage")
            self.assertFalse(Path(stage).exists(), "partial staging files must be removed")
        self.assertEqual(list((partial / "tmp").iterdir()), [])
        self.assert_version(later / ".local/bin/bcmr")
        self.assertEqual(list((later / "tmp").iterdir()), [])
        self.assert_summary(result, installed=1, failed=1)

    def test_remote_download_failure_falls_back_locally_with_pinned_version_and_cleanup(self):
        cross_uname = "Linux x86_64" if platform.system() != "Linux" else "Darwin arm64"
        home = self.host("fallback", uname=cross_uname)
        self.config["hosts"]["fallback"]["http_failure"] = True
        result = self.invoke("fallback")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr")
        downloads = self.calls("curl")
        self.assertTrue(any(row.get("host") == "fallback" for row in downloads),
                        "the remote HTTP transfer must be attempted")
        self.assertTrue(any(row.get("host") is None for row in downloads),
                        "a failed remote transfer must fall back to a local HTTP transfer")
        asset = "bcmr-x86_64-linux.tar.gz" if cross_uname.startswith("Linux") else "bcmr-aarch64-macos.tar.gz"
        for row in downloads:
            url = next((arg for arg in row["args"] if arg.startswith("https://")), "")
            self.assertIn("/releases/download/v" + self.version + "/" + asset, url)
            self.assertNotIn("/latest/", url)
        self.assertEqual(list((home / "tmp").iterdir()), [], "remote staging/extraction must be cleaned")
        self.assertEqual(list(self.local_tmp.iterdir()), [], "local fallback download/extraction must be cleaned")
        self.assert_summary(result, installed=1)

    def test_directory_install_path_fails_without_installing_inside_it(self):
        home = self.host("directory")
        selected = home / "existing-directory"
        selected.mkdir()
        sentinel = selected / "keep.txt"
        sentinel.write_text("preserve this directory\n")
        result = self.invoke("directory", "--path", selected)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertTrue(selected.is_dir())
        self.assertEqual(list(selected.iterdir()), [sentinel], "install must not treat a directory as its destination")
        self.assertEqual(sentinel.read_text(), "preserve this directory\n")
        self.assertEqual(list((home / "tmp").iterdir()), [])

    def test_noninteractive_sudo_denial_does_not_prompt_or_stop_later_host(self):
        denied = self.host("denied", sudo="denied")
        allowed = self.host("allowed")
        result = self.invoke("denied", "allowed", "--sudo")
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertFalse((denied / ".local/bin/bcmr").exists())
        self.assert_version(allowed / ".local/bin/bcmr")
        sudo_calls = self.calls("sudo")
        self.assertTrue(sudo_calls)
        self.assertTrue(all("-n" in row["args"] for row in sudo_calls), sudo_calls)
        self.assertFalse(any(row["stdin_tty"] for row in sudo_calls))
        self.assertRegex(result.stderr.lower(), r"non.?interactive|passwordless|sudo -n")
        self.assert_summary(result, installed=1, failed=1)

    def test_interactive_sudo_inherits_native_ssh_terminal(self):
        home = self.host("terminal", sudo="tty")
        result = self.invoke("terminal", "--sudo", interactive=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr")
        interactive_sudo = [row for row in self.calls("sudo") if "-n" not in row["args"]]
        self.assertTrue(interactive_sudo, result.stderr)
        self.assertTrue(all(row["stdin_tty"] and row["stderr_tty"] for row in interactive_sudo))
        self.assertTrue(any(row["stdin_tty"] and any(arg in ("-t", "-tt", "RequestTTY=force", "RequestTTY=yes")
                                                    for arg in row["args"])
                            for row in self.calls("ssh")), "sudo must use native SSH PTY allocation")
        self.assertFalse(any("-S" in row["args"] or "--stdin" in row["args"] for row in self.calls("sudo")))
        self.assert_summary(result, installed=1)

    def test_json_mode_uses_noninteractive_sudo_even_inside_a_terminal(self):
        home = self.host("json-terminal", sudo="tty")
        result = self.invoke("json-terminal", "--sudo", "--json", interactive=True)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertFalse((home / ".local/bin/bcmr").exists())
        sudo_calls = self.calls("sudo")
        self.assertTrue(sudo_calls)
        self.assertTrue(all("-n" in row["args"] for row in sudo_calls), sudo_calls)

    def test_interactive_sudo_prompt_remains_visible_when_stdout_is_redirected(self):
        home = self.host("split-terminal", sudo="tty")
        redirected = self.root / "stdout.log"
        result = self.invoke("split-terminal", "--sudo", interactive=True, stdout_file=redirected)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(home / ".local/bin/bcmr")
        self.assertIn("FIXTURE_SUDO_PROMPT", result.stderr,
                      "the native remote sudo prompt must remain on the parent's stderr terminal")
        self.assertEqual(redirected.read_bytes(), b"", "interactive SSH output must stay on the terminal")
        self.assert_summary(result, installed=1)

    def test_hostile_install_path_remains_literal(self):
        home = self.host("quoted")
        selected = home / "chosen's $(touch INJECTED)/bcmr"
        result = self.invoke("quoted", "--path", selected)
        self.assertFalse((home / "INJECTED").exists(), "remote path must never execute shell substitutions")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_version(selected)
        self.assert_summary(result, installed=1)

    def test_option_like_ssh_target_is_rejected_before_transport(self):
        result = self.invoke("--", "-oProxyCommand=touch INJECTED")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [], "unsafe targets must be rejected before invoking SSH or SFTP")
        self.assertRegex(result.stderr.lower(), r"target|host|invalid|unexpected")


if __name__ == "__main__":
    unittest.main(verbosity=2)
