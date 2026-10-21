#!/usr/bin/env python3
"""Move preflight must preserve self aliases and symlink referent payloads.

Only OpenSSH transport dispatch/configuration is replaced. Shell filesystem
commands and the OpenSSH SFTP subsystem operate on real isolated files.
"""
import os
from pathlib import Path
import sys
import unittest

import composite_transfer_test as fixture


@unittest.skipUnless(os.name == 'posix', 'requires POSIX shell and filesystem')
class RelayIdentityTests(unittest.TestCase):
    put = fixture.CompositeTransferTests.put
    run_bcmr = fixture.CompositeTransferTests.run_bcmr
    assert_ok = fixture.CompositeTransferTests.assert_ok

    def setUp(self):
        fixture.CompositeTransferTests.setUp(self)
        self.install_transport(False)

    def install_transport(self, sftp_only):
        self.env['IDENTITY_SFTP_ONLY'] = '1' if sftp_only else '0'
        if fixture.SFTP_SERVER:
            self.env['IDENTITY_SFTP_SERVER'] = fixture.SFTP_SERVER
        (self.bin / 'ssh').write_text('''#!/usr/bin/env python3
import os, sys
args = sys.argv[1:]; subsystem = False; config = False; debug = False
while args and args[0].startswith('-'):
    flag = args.pop(0)
    if flag in ('-o', '-p', '-i', '-F', '-J', '-l'):
        value = args.pop(0)
        debug |= flag == '-o' and value == 'LogLevel=DEBUG1'
    elif flag == '-s': subsystem = True
    elif flag == '-G': config = True
    elif flag in ('-v', '-vv', '-vvv'): debug = True
    elif flag == '--': break
    elif flag not in ('-T', '-C'): sys.exit('unexpected SSH option: '+flag)
host, words = args[0], args[1:]
if host not in ('fixture', 'legacy', 'second'): sys.exit('unexpected SSH alias: '+host)
same_peer = host != 'second'
if config:
    print('hostname '+('127.0.0.1' if same_peer else '127.0.0.2'))
    print('user fixture-user\\nport 22\\nidentityfile /fixture/key\\nidentitiesonly yes\\nidentityagent none')
    sys.exit(0)
if debug:
    sys.stderr.write('debug1: Server host key: ssh-ed25519 SHA256:'+('A' if same_peer else 'B')*43+'\\n')
if words == ['-s', 'sftp']: subsystem = True
home = os.environ['STAR_FIXTURE_HOME' if same_peer else 'STAR_SECOND_HOME']
os.chdir(home); os.environ['HOME'] = home
if subsystem and words[-1] == 'sftp':
    os.execv(os.environ['IDENTITY_SFTP_SERVER'], ['sftp-server'])
if os.environ['IDENTITY_SFTP_ONLY'] == '1':
    sys.stderr.write('This service allows sftp connections only.\\n'); sys.exit(1)
os.execv('/bin/sh', ['sh', '-c', ' '.join(words)])
''')

    def assert_rejected_intact(self, result, source, target, content):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue(source.exists() or source.is_symlink())
        self.assertFalse(target.is_symlink())
        self.assertEqual(target.read_text(), content)
        self.assertFalse(list(self.home.glob('.bcmr.*')))
        self.assertFalse(list(self.second.glob('.bcmr.*')))

    def test_same_remote_leaf_relative_and_absolute_is_rejected_before_resume_skip(self):
        source = self.put(self.home, 'payload', 'only remaining bytes')
        original_ctime = source.stat().st_ctime_ns
        result = self.run_bcmr('move', '-C', 'fixture:payload', f'fixture:{source}')
        self.assert_rejected_intact(result, source, source, 'only remaining bytes')
        self.assertEqual(source.stat().st_ctime_ns, original_ctime)

    def test_same_remote_leaf_across_aliases_is_rejected_before_resume_skip(self):
        source = self.put(self.home, 'payload', 'only remaining bytes')
        result = self.run_bcmr('move', '-C', 'fixture:payload', f'legacy:{source}')
        self.assert_rejected_intact(result, source, source, 'only remaining bytes')

    @unittest.skipUnless(sys.platform == 'darwin', 'uses Darwin boot-UUID command boundary')
    def test_ssh_namespace_without_boot_uuid_uses_authenticated_peer_and_inode_guard(self):
        if self.env['IDENTITY_SFTP_ONLY'] == '1':
            self.skipTest('SFTP has no boot UUID/inode probe')
        sysctl = self.bin / 'sysctl'
        sysctl.write_text('#!/bin/sh\nexit 1\n')
        sysctl.chmod(0o755)
        source = self.put(self.home, 'payload', 'fallback peer guard bytes')
        original_ctime = source.stat().st_ctime_ns
        result = self.run_bcmr('move', '-C', 'fixture:payload', f'legacy:{source}')
        self.assert_rejected_intact(result, source, source, 'fallback peer guard bytes')
        self.assertEqual(source.stat().st_ctime_ns, original_ctime)

    def test_same_leaf_through_parent_alias_is_rejected_before_resume_skip(self):
        source = self.put(self.home, 'tree/payload', 'parent alias bytes')
        (self.home / 'alias').symlink_to('tree', target_is_directory=True)
        result = self.run_bcmr('move', '-C', 'fixture:tree/payload', 'legacy:alias/payload')
        self.assert_rejected_intact(result, source, source, 'parent alias bytes')

    def test_link_cannot_replace_its_payload_through_a_different_alias(self):
        target = self.put(self.home, 'payload', 'irreplaceable link referent')
        source = self.home / 'link'
        source.symlink_to('payload')
        result = self.run_bcmr('move', '-f', '-y', 'fixture:link', 'legacy:payload')
        self.assert_rejected_intact(result, source, target, 'irreplaceable link referent')
        self.assertEqual(os.readlink(source), 'payload')

    def test_planned_link_cannot_replace_a_destination_with_a_self_loop(self):
        source = self.home / 'link'
        source.symlink_to('payload')
        self.put(self.home, 'payload', 'source referent')
        target = self.put(self.second, 'payload', 'destination only bytes')
        result = self.run_bcmr('move', '-f', '-y', 'fixture:link', 'second:payload')
        self.assert_rejected_intact(result, source, target, 'destination only bytes')
        self.assertEqual(os.readlink(source), 'payload')

    def test_link_cannot_replace_its_payload_even_when_new_link_resolves_elsewhere(self):
        target = self.put(self.home, 'payload', 'retain this referent')
        source = self.home / 'directory/link'
        source.parent.mkdir()
        source.symlink_to('../payload')
        result = self.run_bcmr('move', '-f', '-y', 'fixture:directory/link', 'legacy:payload')
        self.assert_rejected_intact(result, source, target, 'retain this referent')
        self.assertEqual(os.readlink(source), '../payload')

    def test_safe_distinct_link_entries_sharing_a_referent_still_move(self):
        self.put(self.home, 'payload', 'shared referent retained')
        source = self.home / 'first-link'
        target = self.home / 'second-link'
        source.symlink_to('payload')
        target.symlink_to('payload')
        result = self.run_bcmr('move', '-f', '-y', 'fixture:first-link', 'legacy:second-link')
        if self.env['IDENTITY_SFTP_ONLY'] == '1':
            # Existing SFTP leaf replacement deliberately requires SSH to keep
            # the ACL/xattr preservation contract, independently of identity.
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(source.is_symlink())
        else:
            self.assert_ok(result)
            self.assertFalse(source.is_symlink())
        self.assertEqual(os.readlink(target), 'payload')
        self.assertEqual((self.home / 'payload').read_text(), 'shared referent retained')

    def test_matching_bytes_in_distinct_remote_directories_still_move(self):
        source = self.put(self.home, 'payload', 'ordinary resume bytes')
        target = self.put(self.second, 'payload', 'ordinary resume bytes')
        original = target.stat().st_ino
        result = self.run_bcmr('move', '-C', 'fixture:payload', 'second:payload')
        self.assert_ok(result)
        self.assertFalse(source.exists())
        self.assertEqual(target.read_text(), 'ordinary resume bytes')
        self.assertEqual(target.stat().st_ino, original)


@unittest.skipUnless(fixture.SFTP_SERVER, 'requires real OpenSSH sftp-server')
class SftpRelayIdentityTests(RelayIdentityTests):
    def setUp(self):
        fixture.CompositeTransferTests.setUp(self)
        self.install_transport(True)


if __name__ == '__main__':
    unittest.main()
