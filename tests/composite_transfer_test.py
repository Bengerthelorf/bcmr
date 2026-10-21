#!/usr/bin/env python3
"""Composite transfers use real shell/serve and SFTP subsystem processes."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time
import unittest

import star_copy_test as fixture

SFTP_SERVER = next((str(p) for p in (Path('/usr/libexec/sftp-server'),
                                    Path('/usr/lib/openssh/sftp-server')) if p.is_file()), None)


@unittest.skipUnless(os.name == 'posix', 'requires POSIX shell')
class CompositeTransferTests(unittest.TestCase):
    setUp = fixture.SourceStarTests.setUp
    put = fixture.SourceStarTests.put
    run_bcmr = fixture.SourceStarTests.run_bcmr
    assert_ok = fixture.SourceStarTests.assert_ok
    assert_files = fixture.SourceStarTests.assert_files

    def test_mixed_local_remote_sources_one_destination(self):
        local = self.put(self.local, 'local.txt', 'local bytes')
        self.put(self.home, 'remote.txt', 'remote bytes')
        self.assert_ok(self.run_bcmr('copy', local, 'fixture:remote.txt', self.dest))
        self.assert_files(self.dest, {'local.txt': 'local bytes', 'remote.txt': 'remote bytes'})

    def test_relay_between_hosts_preserves_source(self):
        source = self.put(self.home, 'source.txt', 'client mediated bytes')
        self.assert_ok(self.run_bcmr('copy', '-V', 'fixture:source.txt', 'second:new/landed.txt'))
        self.assertEqual((self.second / 'new/landed.txt').read_text(), source.read_text())
        self.assertTrue(source.exists())

    def test_recursive_relay_with_exclusion_and_preserved_links(self):
        self.put(self.home, 'tree/keep/data', 'selected')
        self.put(self.home, 'tree/skip/secret', 'excluded')
        (self.home / 'tree/empty').mkdir()
        (self.home / 'tree/link').symlink_to('keep/data')
        self.assert_ok(self.run_bcmr('copy', '-r', '--no-deref', '-e', 'skip',
                                    'fixture:tree', 'second:out/'))
        target = self.second / 'out/tree'
        self.assertEqual((target / 'keep/data').read_text(), 'selected')
        self.assertEqual(os.readlink(target / 'link'), 'keep/data')
        self.assertTrue((target / 'empty').is_dir())
        self.assertFalse((target / 'skip').exists())

    def test_mixed_move_removes_only_after_all_success(self):
        local = self.put(self.local, 'one', 'one')
        remote = self.put(self.home, 'two', 'two')
        self.assert_ok(self.run_bcmr('move', local, 'fixture:two', self.dest))
        self.assertFalse(local.exists())
        self.assertFalse(remote.exists())
        self.assert_files(self.dest, {'one': 'one', 'two': 'two'})

    def test_mixed_dangling_local_link_keeps_link_object(self):
        source = self.local / 'dangling'
        source.symlink_to('missing')
        self.put(self.home, 'remote', 'remote')
        self.assert_ok(self.run_bcmr('copy', '--no-deref', source, 'fixture:remote', self.dest))
        self.assertEqual(os.readlink(self.dest / 'dangling'), 'missing')
        self.assertEqual((self.dest / 'remote').read_text(), 'remote')
        self.assertTrue(source.is_symlink())

    def test_late_invalid_mixed_source_leaves_earlier_destination_absent(self):
        local = self.put(self.local, 'one', 'one')
        result = self.run_bcmr('copy', local, 'fixture:missing', self.dest)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(list(self.dest.iterdir()), [])
        self.assertTrue(local.exists())

    def test_relay_dry_run_has_no_destination_writes(self):
        source = self.put(self.home, 'source', 'kept')
        self.assert_ok(self.run_bcmr('move', '-n', 'fixture:source', 'second:new/'))
        self.assertFalse((self.second / 'new').exists())
        self.assertTrue(source.exists())

    def test_relay_rejects_directory_alias_descendant_before_writes(self):
        self.put(self.home, 'tree/data', 'retained')
        (self.home / 'alias').symlink_to('tree', target_is_directory=True)
        result = self.run_bcmr('copy', '-r', 'fixture:tree', 'fixture:alias/new')
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse((self.home / 'tree/new').exists())
        self.assertEqual((self.home / 'tree/data').read_text(), 'retained')

    def test_relay_move_link_cannot_replace_its_own_target(self):
        target = self.put(self.home, 'payload', 'irreplaceable bytes')
        source = self.home / 'link'
        source.symlink_to('payload')
        result = self.run_bcmr('move', '-f', '-y', 'fixture:link', 'fixture:payload')
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(target.is_symlink())
        self.assertEqual(target.read_text(), 'irreplaceable bytes')
        self.assertEqual(os.readlink(source), 'payload')


@unittest.skipUnless(SFTP_SERVER, 'requires a real OpenSSH sftp-server')
class SftpOnlyTests(CompositeTransferTests):
    def setUp(self):
        fixture.SourceStarTests.setUp(self)
        self.events = self.root / 'ssh-events.jsonl'
        self.env.update(SFTP_TEST_SERVER=SFTP_SERVER, SFTP_TEST_EVENTS=str(self.events))
        # This boundary only dispatches transports. All file operations are
        # implemented by the actual OpenSSH SFTP subsystem, never this fixture.
        (self.bin / 'ssh').write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]; subsystem = False
while args and args[0].startswith('-'):
    option = args.pop(0)
    if option in ('-o', '-p', '-i', '-F', '-J', '-l'): args.pop(0)
    elif option == '-s': subsystem = True
    elif option not in ('-T', '-C'): sys.exit('unexpected ssh option '+option)
host, words = args[0], args[1:]
if words == ['-s', 'sftp']: subsystem = True
with open(os.environ['SFTP_TEST_EVENTS'], 'a') as f:
    f.write(json.dumps(dict(host=host, subsystem=subsystem, words=words))+'\\n')
if host not in ('fixture', 'second'): sys.exit('unexpected host')
home = os.environ['STAR_SECOND_HOME' if host == 'second' else 'STAR_FIXTURE_HOME']
os.chdir(home); os.environ['HOME'] = home
if subsystem and words[-1] == 'sftp':
    os.execv(os.environ['SFTP_TEST_SERVER'], ['sftp-server'])
sys.stderr.write('This service allows sftp connections only.\\n'); sys.exit(1)
''')

    def test_forced_sftp_upload_download_without_exec(self):
        src = self.put(self.local, "space '中文\nname", 'sftp bytes')
        self.assert_ok(self.run_bcmr('copy', '--remote-backend=sftp', '-V', src, 'fixture:new/'))
        self.assert_ok(self.run_bcmr('copy', '--remote-backend=sftp', '-V',
                                    'fixture:new/' + src.name, self.dest))
        self.assertEqual((self.dest / src.name).read_text(), 'sftp bytes')
        events = [json.loads(line) for line in self.events.read_text().splitlines()]
        self.assertTrue(events)
        self.assertTrue(all(event['subsystem'] for event in events), events)

    def test_sftp_tilde_and_star(self):
        self.put(self.home, 'tree/report-one', 'one')
        self.put(self.home, 'tree/report-two', 'two')
        self.put(self.home, 'tree/.report-hidden', 'hidden')
        self.assert_ok(self.run_bcmr('copy', '--remote-backend=sftp',
                                    'fixture:~/tree/report-*', self.dest))
        self.assert_files(self.dest, {'report-one': 'one', 'report-two': 'two'})

    def test_sftp_move_preserves_atime_before_cleanup_hash_read(self):
        source = self.put(self.home, 'dated', 'hash before copy must not change -p metadata')
        os.utime(source, (1_500_000_001, 1_500_000_002))
        self.assert_ok(self.run_bcmr('move', '--remote-backend=sftp', '-p', 'fixture:dated', self.dest))
        landed = self.dest / 'dated'
        self.assertEqual(int(landed.stat().st_atime), 1_500_000_001)
        self.assertEqual(int(landed.stat().st_mtime), 1_500_000_002)
        self.assertFalse(source.exists())

    def test_sftp_resume_matching_file_skips_without_replacement(self):
        source = self.put(self.local, 'matching', 'same bytes')
        target = self.put(self.second, 'matching', 'same bytes')
        original = target.stat().st_ino
        self.assert_ok(self.run_bcmr('copy', '--remote-backend=sftp', '-C', source, 'second:matching'))
        self.assertEqual(target.stat().st_ino, original)
        self.assertEqual(target.read_text(), 'same bytes')
        self.assertTrue(source.exists())

    def start_paced_upload(self, operation='copy'):
        source = self.local / 'large'
        source.write_bytes(bytes(range(256)) * 1024)
        target = self.second / 'result'
        process = subprocess.Popen([fixture.BCMR, operation, '--remote-backend=sftp',
                                    '--progress=off', '--bandwidth-limit=64KiB',
                                    str(source), 'second:result'], env=self.env, cwd=self.local,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            stages = list(self.second.glob('.bcmr.upload.*/result'))
            if stages and stages[0].stat().st_size > 0:
                return source, target, process
            if process.poll() is not None:
                self.fail('upload exited before pacing: ' + str(process.communicate()))
            time.sleep(.02)
        self.fail('paced upload never wrote staging bytes')

    def test_sftp_racing_dangling_link_is_preserved(self):
        source, target, process = self.start_paced_upload()
        target.symlink_to('absent-target')
        output, error = process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0, output + error)
        self.assertEqual(os.readlink(target), 'absent-target')
        self.assertTrue(source.exists())
        self.assertEqual(list(self.second.glob('.bcmr.upload.*')), [])

    def test_sftp_interrupt_aborts_publication_and_move_cleanup(self):
        source, target, process = self.start_paced_upload('move')
        process.send_signal(signal.SIGINT)
        output, error = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 130, output + error)
        self.assertTrue(source.exists())
        self.assertFalse(target.exists())
        self.assertEqual(list(self.second.glob('.bcmr.upload.*')), [])


if __name__ == '__main__':
    unittest.main()
