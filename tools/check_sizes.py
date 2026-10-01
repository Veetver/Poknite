#!/usr/bin/env python3
"""Check artifact byte sizes, never confuse them with installed disk usage."""
import argparse
from pathlib import Path

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--release-dir', type=Path)
    parser.add_argument('--require-android', action='store_true')
    args = parser.parse_args()
    release = args.release_dir or args.root / 'target/release'
    targets = [(release / 'poknited', 10), (release / 'poknite', 12)]
    apks = list((args.root / 'android/app/build/outputs/apk/release').glob('*.apk'))
    targets.extend((apk, 5) for apk in apks)
    failed = args.require_android and not apks
    for artifact, limit in targets:
        if not artifact.is_file():
            print(f'{artifact.name}: not built')
            failed = True
            continue
        size = artifact.stat().st_size / 1048576
        print(f'{artifact.name}: {size:.3f} MiB / {limit} MiB')
        failed |= size > limit
    raise SystemExit(1 if failed else 0)

if __name__ == '__main__':
    main()
