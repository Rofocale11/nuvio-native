#!/usr/bin/env python3
"""Verify EVERY ELF in the actual IPK against byte-bound link receipts.

Two kinds of ELF are accepted:

* receipted — the Rust link path (`ci/arm-cc.py`) emitted a `.link.json` + linker map + trace,
  and the receipt still matches the shipped bytes (directly or through a verified strip).
* digest-pinned (`--unreceipted FILE`) — an ELF built outside that path (today: the CMake-built
  `libsbengine.so`). The file lists the exact bytes that went into this package,
  `sha256sum`-style; a mismatch, or a digest for a file the package does not contain, fails.
  This is not a weaker check invented at packaging time: it pins what `ci/build-sbengine.sh`
  produced, and it exists because that artifact has no Rust-side link receipt to bind.
"""
import argparse
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile

spec = importlib.util.spec_from_file_location('evidence', Path(__file__).with_name('check-link-evidence.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

def read_unreceipted(path):
    pinned = {}
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        digest, _, name = line.partition(' ')
        name = name.strip().lstrip('*')
        if len(digest) != 64 or not name:
            raise ValueError(f'{path}: expected "<sha256>  <name>"')
        pinned[name] = digest
    return pinned

def check(ipk, evidence, unreceipted=None):
    raw = ipk.read_bytes()
    if not raw.startswith(b'!<arch>\n'):
        raise ValueError('not ar')
    pos, data = 8, None
    while pos < len(raw):
        header = raw[pos:pos + 60]
        if len(header) != 60 or header[58:] != b'`\n':
            raise ValueError('malformed ar')
        size = int(header[48:58]); name = header[:16].decode().strip().rstrip('/')
        blob = raw[pos + 60:pos + 60 + size]
        if len(blob) != size:
            raise ValueError('truncated ar')
        if name == 'data.tar.gz':
            if data is not None: raise ValueError('duplicate data archive')
            data = blob
        pos += 60 + size + size % 2
    if data is None: raise ValueError('missing data archive')
    pinned = dict(unreceipted or {})
    found, digests = set(), {}
    with tempfile.TemporaryDirectory() as d, tarfile.open(fileobj=io.BytesIO(data)) as t:
        for member in t:
            if not member.isfile(): continue
            blob = t.extractfile(member).read()
            if blob[:4] != b'\x7fELF': continue
            name = Path(member.name).name
            if name in found: raise ValueError('duplicate ELF basename')
            found.add(name)
            digests[name] = hashlib.sha256(blob).hexdigest()
            if name in pinned:
                continue  # digest-pinned below, has no link receipt
            elf = Path(d) / name; elf.write_bytes(blob)
            m.check_elf(elf, evidence / name)
    for name, want in pinned.items():
        if name not in found:
            raise ValueError(f'{name}: digest-pinned but not present in the IPK')
        if digests[name] != want:
            raise ValueError(f'{name}: SHA-256 does not match the build manifest')
        found.discard(name)
    expected = {p.name[:-len('.link.json')] for p in evidence.glob('*.link.json')}
    if not found or found != expected:
        raise ValueError(f'ELF closure mismatch: actual {sorted(found)}, receipts {sorted(expected)}')
    print(f'PASS actual IPK: {len(found)} receipted + {len(pinned)} digest-pinned ELF files verified')

if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('ipk', type=Path); p.add_argument('evidence', type=Path)
    p.add_argument('--unreceipted', type=Path, metavar='FILE',
                   help='sha256sum-style list of ELFs accepted by digest instead of a link receipt')
    a = p.parse_args()
    check(a.ipk, a.evidence, read_unreceipted(a.unreceipted) if a.unreceipted else None)

