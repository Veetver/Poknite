#!/usr/bin/env python3
"""Reject tags that do not match the versions embedded in the release builds."""
import argparse
import os
from pathlib import Path
import re
import tomllib


def release_metadata(root: Path, tag: str) -> dict[str, str]:
    number = r"(?:0|[1-9][0-9]*)"
    if not re.fullmatch(rf"v{number}\.{number}\.{number}(?:-(?:alpha|beta|rc)\.{number})?", tag):
        raise ValueError("Expected vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-rc.N/alpha.N/beta.N")
    version = tag[1:]
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    android = (root / "android/app/build.gradle.kts").read_text()
    name = re.search(r'\bversionName\s*=\s*"([^"]+)"', android)
    code = re.search(r"\bversionCode\s*=\s*([0-9]+)", android)
    if cargo["workspace"]["package"]["version"] != version or not name or name[1] != version:
        raise ValueError("Tag must match Cargo.toml workspace version and Android versionName")
    if not code or not 1 <= int(code[1]) <= 2100000000:
        raise ValueError("Android versionCode must be in 1..2100000000")
    return {"version": version, "prerelease": str("-" in version).lower()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        metadata = release_metadata(args.root, args.tag)
    except ValueError as error:
        parser.exit(1, f"Release validation failed: {error}\n")
    for key, value in metadata.items():
        print(f"{key}={value}")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a") as stream:
            for key, value in metadata.items():
                stream.write(f"{key}={value}\n")


if __name__ == "__main__":
    main()
