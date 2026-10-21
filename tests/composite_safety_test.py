#!/usr/bin/env python3
"""Composite boundary regressions through real shell and subsystem processes."""
import os
import signal
import subprocess
import time
import unittest

import star_copy_test as fixture
import composite_transfer_test as composite


@unittest.skipUnless(os.name == 'posix', 'requires POSIX shell')
class CompositeSafetyTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    def test_move_colliding_basenames_refuses_even_force(self):
        local = self.put(self.local, 'duplicate', 'local must survive')
        remote = self.put(self.home, 'duplicate', 'remote must survive')
        result = self.run_bcmr('move', '-f', '-y', local, 'fixture:duplicate', self.dest)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(local.read_text(), 'local must survive')
        self.assertEqual(remote.read_text(), 'remote must survive')
        self.assertEqual(list(self.dest.iterdir()), [])

    def test_relay_dash_filename_is_payload_not_cat_option(self):
        self.put(self.home, '-n', 'real dash filename bytes')
        self.assert_ok(self.run_bcmr('copy', '-V', 'fixture:-n', 'second:result'))
        self.assertEqual((self.second / 'result').read_text(), 'real dash filename bytes')

    def test_ordinary_move_link_cannot_replace_shared_local_payload(self):
        for upload in (True, False):
            with self.subTest(upload=upload):
                root = self.local if upload else self.home
                payload = self.put(root, 'payload', 'only referent bytes')
                source = root / 'directory' / 'link'
                source.parent.mkdir(exist_ok=True)
                source.symlink_to('../payload')
                result = self.run_bcmr('move', '-f', '-y',
                                       source if upload else 'fixture:directory/link',
                                       'fixture:' + str(payload) if upload else payload)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(payload.is_symlink())
                self.assertEqual(payload.read_text(), 'only referent bytes')
                self.assertEqual(os.readlink(source), '../payload')

    def test_interrupt_during_dry_run_preflight_is_not_success(self):
        self.put(self.home, 'source', 'kept')
        marker = self.root / 'preflight'
        self.env['COMPOSITE_PREFLIGHT_MARKER'] = str(marker)
        transport = self.bin / 'ssh'
        script = transport.read_text().replace("os.execv('/bin/sh',", """if 'bcmr-relay-identity' in ' '.join(words):
    import time
    with open(os.environ['COMPOSITE_PREFLIGHT_MARKER'], 'w') as f: f.write('checking')
    time.sleep(.5)
os.execv('/bin/sh',""", 1)
        transport.write_text(script)
        process = subprocess.Popen([fixture.BCMR, 'copy', '-n', '--progress=off',
                                    'fixture:source', 'fixture:new'], env=self.env,
                                   cwd=self.local, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        deadline = time.monotonic() + 10
        while not marker.exists():
            if process.poll() is not None:
                self.fail('copy exited before preflight: ' + str(process.communicate()))
            if time.monotonic() > deadline:
                self.fail('preflight marker absent')
            time.sleep(.01)
        process.send_signal(signal.SIGINT)
        output, error = process.communicate(timeout=10)
        self.assertEqual(process.returncode, 130, output + error)
        self.assertFalse((self.home / 'new').exists())

    def test_composite_rejects_explicit_force_strategies(self):
        local = self.put(self.local, 'local', 'one')
        self.put(self.home, 'remote', 'two')
        for strategy in ('--reflink=force', '--sparse=always'):
            with self.subTest(strategy=strategy):
                result = self.run_bcmr('copy', strategy, '--bandwidth-limit=1MiB',
                                       local, 'fixture:remote', self.dest)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(list(self.dest.iterdir()), [])

    def test_interrupt_last_link_publication_keeps_move_source(self):
        source = self.home / 'link'
        source.symlink_to('unavailable-target')
        marker = self.root / 'committing'
        self.env['COMPOSITE_COMMIT_MARKER'] = str(marker)
        transport = self.bin / 'ssh'
        script = transport.read_text()
        script = script.replace("os.execv('/bin/sh',", """if 'bcmr-upload-commit' in ' '.join(words):
    import time
    with open(os.environ['COMPOSITE_COMMIT_MARKER'], 'w') as f: f.write('committing')
    time.sleep(1)
os.execv('/bin/sh',""", 1)
        transport.write_text(script)
        process = subprocess.Popen([fixture.BCMR, 'move', '--progress=off',
                                    'fixture:link', 'second:link'], env=self.env,
                                   cwd=self.local, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        deadline = time.monotonic() + 10
        while not marker.exists():
            if process.poll() is not None:
                self.fail('move exited before publication: ' + str(process.communicate()))
            if time.monotonic() > deadline:
                self.fail('publication marker absent')
            time.sleep(.02)
        process.send_signal(signal.SIGINT)
        out, err = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 130, out + err)
        self.assertTrue(source.is_symlink())
        self.assertEqual(os.readlink(source), 'unavailable-target')


@unittest.skipUnless(composite.SFTP_SERVER, 'requires real OpenSSH sftp-server')
class SftpChannelDenialTests(unittest.TestCase):
    setUp = composite.SftpOnlyTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok

    def test_auto_sftp_after_exit_255_exec_channel_denial(self):
        source = self.put(self.home, 'restricted', 'subsystem bytes')
        transport = self.bin / 'ssh'
        script = transport.read_text().replace(
            "sys.stderr.write('This service allows sftp connections only.\\n'); sys.exit(1)",
            "sys.stderr.write('exec request failed on channel 0\\n'); sys.exit(255)")
        transport.write_text(script)
        self.assert_ok(self.run_bcmr('copy', 'fixture:restricted', self.dest))
        self.assertEqual((self.dest / 'restricted').read_text(), source.read_text())


if __name__ == '__main__':
    unittest.main()
