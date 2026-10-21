#!/usr/bin/env python3
"""Staging cleanup never follows retargeted parents or replaced private data.

Filesystem operations are served by the actual local OpenSSH sftp-server.
Only the SSH authentication/transport boundary uses the shared test fixture.
"""
import os
import subprocess
import sys
import time
import unittest

import composite_transfer_test as sftp_fixture
import star_copy_test as fixture


@unittest.skipUnless(os.name == 'posix' and sftp_fixture.SFTP_SERVER,
                     'requires POSIX and the real OpenSSH sftp-server')
class SftpStageTests(unittest.TestCase):
    setUp = sftp_fixture.SftpOnlyTests.setUp
    put = fixture.SourceStarTests.put

    def paced_upload(self, parent='parent', preserve=False):
        source = self.local / 'large'
        source.write_bytes(bytes(range(256)) * 1024)
        args = [fixture.BCMR, 'copy', '--remote-backend=sftp', '--progress=off',
                '--bandwidth-limit=64KiB']
        if preserve:
            source.chmod(0o604)
            os.utime(source, (1_500_000_001, 1_500_000_002))
            args.append('-p')
        process = subprocess.Popen([*args, str(source), f'second:{parent}/result'],
                                   env=self.env, cwd=self.local,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(lambda: process.kill() if process.poll() is None else None)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            stages = list((self.second / parent).glob('.bcmr.upload.*/result'))
            if stages and stages[0].stat().st_size > 0:
                return source, process, stages[0]
            if process.poll() is not None:
                self.fail('upload exited before pacing: ' + str(process.communicate()))
            time.sleep(.01)
        self.fail('paced upload never produced staging bytes')

    def assert_refused(self, process):
        output, error = process.communicate(timeout=15)
        self.assertNotEqual(process.returncode, 0, output + error)
        return error

    def test_retargeted_parent_cleanup_preserves_same_named_unrelated_payload(self):
        original = self.second / 'original'
        spare = self.second / 'spare'
        original.mkdir()
        spare.mkdir()
        alias = self.second / 'parent'
        alias.symlink_to('original', target_is_directory=True)
        source, process, payload = self.paced_upload()
        stage_name = payload.parent.name
        unrelated = spare / stage_name / 'result'
        unrelated.parent.mkdir(mode=0o700)
        unrelated.write_text('unrelated content under a retargeted parent')
        alias.unlink()
        alias.symlink_to('spare', target_is_directory=True)
        self.assert_refused(process)
        self.assertTrue(source.exists())
        self.assertTrue(unrelated.exists(), 'cleanup followed the retargeted parent and deleted unrelated data')
        self.assertEqual(unrelated.read_text(), 'unrelated content under a retargeted parent')
        self.assertFalse((original / 'result').exists())
        self.assertFalse((spare / 'result').exists())

    def test_replaced_private_directory_is_preserved_and_never_published(self):
        parent = self.second / 'parent'
        parent.mkdir()
        _, process, payload = self.paced_upload()
        stage = payload.parent
        saved = parent / 'saved-original-stage'
        stage.rename(saved)
        stage.mkdir(mode=0o700)
        unrelated = stage / 'result'
        unrelated.write_text('unrelated replacement private directory')
        self.assert_refused(process)
        self.assertTrue(unrelated.exists(), 'cleanup deleted data in a replacement private directory')
        self.assertEqual(unrelated.read_text(), 'unrelated replacement private directory')
        self.assertFalse((parent / 'result').exists(), 'replacement staging data was published')
        self.assertTrue((saved / 'result').exists())

    def test_replaced_payload_is_preserved_and_never_published(self):
        parent = self.second / 'parent'
        parent.mkdir()
        _, process, payload = self.paced_upload()
        payload.unlink()
        payload.write_text('unrelated replacement payload')
        self.assert_refused(process)
        self.assertTrue(payload.exists(), 'cleanup deleted an unrelated replacement payload')
        self.assertEqual(payload.read_text(), 'unrelated replacement payload')
        self.assertFalse((parent / 'result').exists(), 'replacement payload was published')

    def test_preserved_metadata_never_follows_a_replaced_payload_symlink(self):
        parent = self.second / 'parent'
        parent.mkdir()
        victim = self.second / 'unrelated-file'
        victim.write_text('unrelated file must retain its metadata')
        victim.chmod(0o640)
        os.utime(victim, (1_600_000_001, 1_600_000_002))
        before = victim.stat()
        _, process, payload = self.paced_upload(preserve=True)
        payload.unlink()
        payload.symlink_to(victim)
        output, error = process.communicate(timeout=15)
        after = victim.stat()
        self.assertEqual((after.st_mode, after.st_atime_ns, after.st_mtime_ns),
                         (before.st_mode, before.st_atime_ns, before.st_mtime_ns),
                         'preserved metadata followed an unrelated replacement symlink')
        self.assertNotEqual(process.returncode, 0, output + error)
        self.assertEqual(victim.read_text(), 'unrelated file must retain its metadata')
        self.assertTrue(payload.is_symlink())
        self.assertFalse((parent / 'result').exists())

    def test_ordinary_preserved_upload_still_removes_owned_staging(self):
        parent = self.second / 'parent'
        parent.mkdir()
        source, process, _ = self.paced_upload(preserve=True)
        output, error = process.communicate(timeout=15)
        self.assertEqual(process.returncode, 0, output + error)
        result = parent / 'result'
        self.assertEqual(result.stat().st_mode & 0o777, 0o604)
        self.assertEqual(int(result.stat().st_mtime), 1_500_000_002)
        self.assertEqual(int(result.stat().st_atime), 1_500_000_001)
        self.assertEqual(result.read_bytes(), source.read_bytes())
        self.assertEqual(list(parent.glob('.bcmr.upload.*')), [])

    @unittest.skipUnless(sys.platform == 'darwin', 'uses native macOS source read ACL')
    def test_preserved_unreadable_final_mode_keeps_times_and_removes_staging(self):
        # The source ACL permits reading even though its portable mode does not.
        # SFTP receives that final mode without the local source ACL, so commit
        # must finish content validation before applying the unreadable mode.
        import pwd
        source = self.local / 'acl-source'
        content = b'content remains readable through the validated staging handle'
        source.write_bytes(content)
        owner = pwd.getpwuid(os.getuid()).pw_name
        acl = subprocess.run(['/bin/chmod', '+a', f'user:{owner} allow read', str(source)],
                             text=True, capture_output=True)
        self.assertEqual(acl.returncode, 0, acl.stdout + acl.stderr)
        parent = self.second / 'parent'
        parent.mkdir()
        for mode in (0o000, 0o200):
            with self.subTest(mode=oct(mode)):
                source.chmod(mode)
                self.assertEqual(source.read_bytes(), content)
                os.utime(source, (1_500_000_001, 1_500_000_002))
                result = parent / f'final-{mode:o}'
                copied = subprocess.run([fixture.BCMR, 'copy', '--remote-backend=sftp',
                                         '--progress=off', '-p', str(source),
                                         'second:parent/' + result.name],
                                        env=self.env, cwd=self.local,
                                        text=True, capture_output=True, timeout=15)
                self.assertEqual(copied.returncode, 0, copied.stdout + copied.stderr)
                attrs = result.stat()
                self.assertEqual(attrs.st_mode & 0o777, mode)
                self.assertEqual((int(attrs.st_atime), int(attrs.st_mtime)),
                                 (1_500_000_001, 1_500_000_002))
                self.assertEqual(list(parent.glob('.bcmr.upload.*')), [])
                result.chmod(0o600)
                self.assertEqual(result.read_bytes(), content)

    @unittest.skipUnless(sys.platform == 'darwin', 'uses native macOS source read ACL')
    def test_move_preserved_unreadable_final_mode_is_refused_before_any_destination_write(self):
        import pwd
        owner = pwd.getpwuid(os.getuid()).pw_name
        parent = self.second / 'parent'
        parent.mkdir()
        content = b'only remove source after validating its unreadable destination'
        for mode in (0o000, 0o200):
            with self.subTest(mode=oct(mode)):
                source = self.local / f'acl-move-{mode:o}'
                source.write_bytes(content)
                acl = subprocess.run(['/bin/chmod', '+a', f'user:{owner} allow read', str(source)],
                                     text=True, capture_output=True)
                self.assertEqual(acl.returncode, 0, acl.stdout + acl.stderr)
                source.chmod(mode)
                os.utime(source, (1_500_000_001, 1_500_000_002))
                result = parent / source.name
                moved = subprocess.run([fixture.BCMR, 'move', '--remote-backend=sftp',
                                        '--progress=off', '-p', str(source),
                                        'second:parent/' + result.name],
                                       env=self.env, cwd=self.local,
                                       text=True, capture_output=True, timeout=15)
                self.assertNotEqual(moved.returncode, 0, moved.stdout + moved.stderr)
                self.assertTrue(source.exists(), 'refused move must retain its source')
                self.assertFalse(result.exists(), 'refusal happened after destination publication')
                self.assertEqual(list(parent.iterdir()), [],
                                 'preflight refusal must not create destination entries or staging')
                self.assertIn('cannot validate final', moved.stderr.lower())
                self.assertEqual(source.stat().st_mode & 0o777, mode)
                self.assertEqual(source.read_bytes(), content)

    @unittest.skipUnless(sys.platform == 'darwin', 'uses native macOS source directory ACL')
    def test_move_preserved_unsearchable_directory_is_refused_before_any_destination_write(self):
        import pwd
        source = self.local / 'acl-directory'
        source.mkdir()
        self.addCleanup(lambda: source.chmod(0o700) if source.exists() else None)
        child = source / 'child'
        content = b'readable child inside an ACL-searchable mode000 directory'
        child.write_bytes(content)
        child.chmod(0o644)
        owner = pwd.getpwuid(os.getuid()).pw_name
        acl = subprocess.run(['/bin/chmod', '+a', f'user:{owner} allow list,search,readattr', str(source)],
                             text=True, capture_output=True)
        self.assertEqual(acl.returncode, 0, acl.stdout + acl.stderr)
        source.chmod(0o000)
        # Establish that a failure cannot be explained by an unreadable local
        # fixture. The portable mode would lose these ACL traversal rights at
        # the SFTP destination, preventing post-quarantine validation there.
        self.assertEqual(source.stat().st_mode & 0o777, 0)
        self.assertEqual(list(source.iterdir()), [child])
        self.assertEqual(child.read_bytes(), content)
        moved = subprocess.run([fixture.BCMR, 'move', '--remote-backend=sftp',
                                '--progress=off', '-r', '-p', str(source),
                                'second:new-destination/'],
                               env=self.env, cwd=self.local,
                               text=True, capture_output=True, timeout=15)
        self.assertNotEqual(moved.returncode, 0, moved.stdout + moved.stderr)
        self.assertEqual(list(self.second.iterdir()), [],
                         'preflight refusal must not create any destination entry')
        self.assertIn('cannot validate final', moved.stderr.lower())
        self.assertTrue(source.is_dir())
        self.assertEqual(source.stat().st_mode & 0o777, 0)
        self.assertEqual(child.stat().st_mode & 0o777, 0o644)
        self.assertEqual(child.read_bytes(), content)


if __name__ == '__main__':
    unittest.main(verbosity=2)
