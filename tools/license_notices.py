#!/usr/bin/env python3
"""Collect cached notices for the two shipped native target dependency graphs."""
import argparse
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tomllib

parser = argparse.ArgumentParser()
parser.add_argument('--cargo-home', type=Path, default=Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))))
parser.add_argument('--output', type=Path, default=Path('THIRD_PARTY_NOTICES.txt'))
args = parser.parse_args()
active = set()
for target, packages in [('x86_64-unknown-linux-gnu', ['poknite-server', 'poknite-desktop']),
                         ('x86_64-pc-windows-gnu', ['poknite-desktop'])]:
    command = ['cargo', 'tree', '--locked', '--offline', '--target', target,
               '--edges', 'normal', '--prefix', 'none', '--format', '{p}']
    for package in packages:
        command.extend(['-p', package])
    tree = subprocess.check_output(command, text=True)
    for line in tree.splitlines():
        match = re.match(r'(\S+) v(\S+)', line)
        if match and not match[1].startswith('poknite'):
            active.add(match.groups())

parts = ['Poknite — third-party notices\n\nNative runtime dependency graphs: Linux x64 and Windows x64.\n'
         'Duplicate identical license texts reference their first occurrence and SHA-256.\n']
seen = {}
for name, version in sorted(active):
    matches = list((args.cargo_home / 'registry/src').glob('*/' + name + '-' + version))
    if not matches:
        raise RuntimeError(f'Fetch cached source first: {name} {version}')
    directory = matches[0]
    metadata = tomllib.loads((directory / 'Cargo.toml').read_text())['package']
    parts.append(f"\n{'=' * 72}\n{name} {version} — {metadata.get('license', 'see notice')}\n"
                 f"{metadata.get('repository', '')}\n")
    files = [f for f in directory.iterdir() if f.is_file() and
             f.name.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'COPYRIGHT', 'NOTICE'))]
    if name == 'fltk-sys':
        files.extend(directory / entry for entry in ['cfltk/LICENSE', 'cfltk/fltk/COPYING', 'cfltk/fltk/src/xutf8/COPYING'])
    if name == 'libsqlite3-sys':
        parts.append('Bundled SQLite C library: public domain (https://sqlite.org/copyright.html).\n')
    if name == 'asn1-rs-impl' and not files:
        # This subcrate declares the same dual license as its containing repository,
        # but its crates.io archive omits the repository-level license files.
        sibling = next((args.cargo_home / 'registry/src').glob('*/asn1-rs-0.7.2'))
        files = sorted(sibling.glob('LICENSE*'))
        parts.append('Repository license texts supplied by the containing asn1-rs archive.\n')
    if not files:
        raise RuntimeError(f'Missing license text: {name} {version}')
    for file in sorted(files):
        content = file.read_text(errors='replace')
        digest = hashlib.sha256(content.encode()).hexdigest()
        path = file.relative_to(directory) if file.is_relative_to(directory) else Path(file.parent.name) / file.name
        label = f'{name} {version}/{path}'
        if digest in seen:
            parts.append(f'License {file.name}: identical to {seen[digest]} (SHA-256 {digest}).\n')
        else:
            seen[digest] = label
            parts.append(f'\n{label}\nSHA-256 {digest}\n{content}\n')
android = Path('android/THIRD_PARTY_NOTICES.txt')
if android.exists():
    parts.append('\n' + '=' * 72 + '\n' + android.read_text())
runtime = Path('deploy/CPP_RUNTIME_NOTICES.txt')
if runtime.exists():
    parts.append('\n' + '=' * 72 + '\n' + runtime.read_text())
args.output.write_text(''.join(parts))
print(f'{len(active)} native dependency notices, {args.output.stat().st_size} bytes')
