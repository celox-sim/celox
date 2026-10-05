#!/usr/bin/env python3
"""Offline positive and adversarial tests for the evidence integrity verifier."""
import importlib.util
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location('verify_evidence', Path(__file__).resolve().parent / 'verify_evidence.py')
v = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(v)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rv32i-evidence-tests-')
        self.root = Path(self.temp.name)
        self.evidence = self.root / 'evidence'
        self.evidence.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def fixture(self, tar_name='run/a.txt', kind=tarfile.REGTYPE, data=b'hello\n',
                expected_data=b'hello\n', duplicate=False, members=None):
        if members is None:
            members = {'run/a.txt': expected_data}
        entries = [(tar_name, data, kind)] if len(members) == 1 else [(n, d, tarfile.REGTYPE) for n, d in members.items()]
        archive = self.evidence / 'test.tar.xz'
        with tarfile.open(archive, 'w:xz', format=tarfile.PAX_FORMAT) as tar:
            for name, payload, typ in entries + (entries if duplicate else []):
                info = tarfile.TarInfo(name)
                info.type = typ
                info.mode = 0o644
                info.size = len(payload) if typ == tarfile.REGTYPE else 0
                if typ in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                    info.linkname = '../../escape'
                tar.addfile(info, io.BytesIO(payload) if typ == tarfile.REGTYPE else None)
        mf = {'schema': 'rv32i-evidence-members-v1', 'members': [
            {'path': n, 'size': len(d), 'sha256': v.digest(d)} for n, d in members.items()]}
        (self.evidence / 'test.members.json').write_text(json.dumps(mf))
        maps = []
        for name, payload in members.items():
            if name.endswith('.json'):
                maps.extend(desc for desc, hashes in v.artifact_maps(json.loads(payload), name))
        manifest = {'schema': 'rv32i-evidence-bundle-v1', 'saved_reports_create_proof_handles': False,
                    'archives': [{'id': 'test', 'file': archive.name,
                                  'sha256': v.file_digest(archive), 'size': archive.stat().st_size,
                                  'member_manifest': 'test.members.json',
                                  'member_manifest_sha256': v.file_digest(self.evidence / 'test.members.json'),
                                  'member_count': len(members), 'uncompressed_bytes': sum(map(len, members.values()))}],
                    'relocation_map': [{'archive_id': 'test', 'original_root': str(self.root / 'original'),
                                        'archive_prefix': 'run', 'scope': 'complete-tree'}],
                    'total_member_count': len(members), 'total_uncompressed_bytes': sum(map(len, members.values())),
                    'recorded_artifact_maps': sorted(maps, key=lambda x: x['document'])}
        (self.evidence / 'manifest.json').write_text(json.dumps(manifest))
        return manifest

    def reject(self, **kwargs):
        self.fixture(**kwargs)
        destination = self.root / 'output'
        with self.assertRaises((ValueError, OSError, tarfile.TarError, EOFError)):
            v.verify_bundle(self.evidence, extract=str(destination))
        self.assertFalse(destination.exists(), 'Failed validation must not begin extraction')

    def test_valid_verification_extraction_and_originals(self):
        self.fixture()
        original = self.root / 'original'
        original.mkdir()
        (original / 'a.txt').write_bytes(b'hello\n')
        output = self.root / 'output'
        result = v.verify_bundle(self.evidence, str(output), True)
        self.assertEqual(result['members_verified'], 1)
        self.assertEqual(result['original_files_verified'], 1)
        self.assertEqual((output / 'run/a.txt').read_bytes(), b'hello\n')

    def test_recorded_artifact_map(self):
        members = {'run/a.txt': b'hello\n', 'run/report.json': json.dumps({'artifacts': {'a.txt': v.digest(b'hello\n')}}).encode()}
        self.fixture(members=members)
        self.assertEqual(v.verify_bundle(self.evidence)['recorded_artifact_references_verified'], 1)

    def test_artifact_map_mismatch(self):
        self.reject(members={'run/a.txt': b'hello\n', 'run/report.json': json.dumps({'artifacts': {'a.txt': '0'*64}}).encode()})

    def test_path_traversal(self): self.reject(tar_name='../escape')
    def test_absolute_path(self): self.reject(tar_name='/tmp/escape')
    def test_windows_path(self): self.reject(tar_name='C:\\escape')
    def test_empty_path_segment(self): self.reject(tar_name='run//a.txt')
    def test_dot_path_segment(self): self.reject(tar_name='run/./a.txt')
    def test_symlink(self): self.reject(kind=tarfile.SYMTYPE)
    def test_hardlink(self): self.reject(kind=tarfile.LNKTYPE)
    def test_fifo(self): self.reject(kind=tarfile.FIFOTYPE)
    def test_character_device(self): self.reject(kind=tarfile.CHRTYPE)
    def test_block_device(self): self.reject(kind=tarfile.BLKTYPE)
    def test_directory(self): self.reject(kind=tarfile.DIRTYPE)
    def test_duplicate_archive_member(self): self.reject(duplicate=True)
    def test_member_hash_mismatch(self): self.reject(data=b'bad!!\n')
    def test_elf_binary(self): self.reject(data=b'\x7fELFxx', expected_data=b'\x7fELFxx')
    def test_pe_binary(self): self.reject(data=b'MZxxxx', expected_data=b'MZxxxx')
    def test_nul_binary(self): self.reject(data=b'x\0xxxx', expected_data=b'x\0xxxx')
    def test_private_key(self):
        synthetic = b'-----BEGIN ' + b'PRIVATE KEY-----'
        self.reject(data=synthetic, expected_data=synthetic)

    def test_cache_filename(self):
        self.reject(tar_name='run/__pycache__/a.pyc', members={'run/__pycache__/a.pyc': b'hello\n'})

    def test_credentials_filename(self):
        self.reject(tar_name='run/credentials.json', members={'run/credentials.json': b'{}'})

    def test_corrupt_archive(self):
        self.fixture()
        p = self.evidence / 'test.tar.xz'
        data = bytearray(p.read_bytes()); data[-1] ^= 1; p.write_bytes(data)
        with self.assertRaisesRegex(ValueError, 'Archive SHA256 mismatch'):
            v.verify_bundle(self.evidence)

    def test_corrupt_manifest(self):
        self.fixture()
        p = self.evidence / 'test.members.json'; p.write_bytes(p.read_bytes() + b' ')
        with self.assertRaisesRegex(ValueError, 'Member manifest hash mismatch'):
            v.verify_bundle(self.evidence)

    def test_existing_extract_destination(self):
        self.fixture()
        with self.assertRaisesRegex(ValueError, 'must not exist'):
            v.verify_bundle(self.evidence, str(self.root))

    def test_extract_inside_repo(self):
        repo = Path(v.__file__).resolve().parents[2]
        with self.assertRaisesRegex(ValueError, 'outside the repository'):
            v.extraction_destination(str(repo / 'new-test-extraction-forbidden'), repo)

    def test_symlink_extract_parent(self):
        self.fixture(); (self.root / 'link').symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, 'Symlink extraction parent'):
            v.verify_bundle(self.evidence, str(self.root / 'link/output'))

    def test_symlink_archive_file(self):
        self.fixture(); p = self.evidence / 'test.tar.xz'; p.rename(self.root / 'real.tar.xz'); p.symlink_to(self.root / 'real.tar.xz')
        with self.assertRaises(OSError): v.verify_bundle(self.evidence)

    def test_duplicate_json_key(self):
        with self.assertRaisesRegex(ValueError, 'Duplicate JSON key'):
            v.parse_json('{"a": 1, "a": 2}')

    def test_original_extra_file(self):
        self.fixture(); p = self.root / 'original'; p.mkdir(); (p / 'a.txt').write_bytes(b'hello\n'); (p / 'extra').write_bytes(b'x')
        with self.assertRaisesRegex(ValueError, 'Original file set differs'):
            v.verify_bundle(self.evidence, check_originals=True)


if __name__ == '__main__':
    unittest.main(verbosity=2)
