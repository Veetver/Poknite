#!/usr/bin/env python3
"""Align and sign a release APK, then verify its pinned signing certificate."""
import argparse
import base64
import os
from pathlib import Path
import re
import subprocess
import tempfile


def fingerprint(value: str) -> str:
    result = value.replace(":", "").strip().lower()
    if not re.fullmatch(r"[0-9a-f]{64}", result):
        raise ValueError("ANDROID_CERT_SHA256 must contain a SHA-256 certificate fingerprint")
    return result


def sign_apk(source: Path, output: Path, build_tools: Path):
    required = ("ANDROID_KEYSTORE_BASE64", "ANDROID_KEYSTORE_PASSWORD", "ANDROID_KEY_ALIAS",
                "ANDROID_KEY_PASSWORD", "ANDROID_CERT_SHA256")
    missing = [name for name in required if not os.environ.get(name)]
    if missing:
        raise ValueError("Missing Android signing configuration: " + ", ".join(missing))
    expected = fingerprint(os.environ["ANDROID_CERT_SHA256"])
    keystore = base64.b64decode(os.environ["ANDROID_KEYSTORE_BASE64"], validate=True)
    output.parent.mkdir(parents=True, exist_ok=True)
    # TemporaryDirectory is private (0700) and removes the key even on failure.
    with tempfile.TemporaryDirectory(prefix="poknite-sign-", dir=os.environ.get("RUNNER_TEMP")) as task_dir:
        task_dir = Path(task_dir)
        key = task_dir / "release.p12"
        key.write_bytes(keystore)
        key.chmod(0o600)
        aligned = task_dir / "aligned.apk"
        signed = task_dir / "signed.apk"
        subprocess.run([str(build_tools / "zipalign"), "-f", "-p", "4", str(source), str(aligned)], check=True)
        subprocess.run([
            str(build_tools / "apksigner"), "sign", "--ks", str(key),
            "--ks-key-alias", os.environ["ANDROID_KEY_ALIAS"],
            "--ks-pass", "env:ANDROID_KEYSTORE_PASSWORD", "--key-pass", "env:ANDROID_KEY_PASSWORD",
            "--v4-signing-enabled", "false", "--out", str(signed), str(aligned),
        ], check=True)
        verification = subprocess.run([
            str(build_tools / "apksigner"), "verify", "--verbose", "--print-certs", str(signed),
        ], check=True, capture_output=True, text=True)
        digests = re.findall(r"Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F]+)", verification.stdout)
        if len(digests) != 1 or fingerprint(digests[0]) != expected:
            raise ValueError("APK signing certificate differs from ANDROID_CERT_SHA256")
        if signed.stat().st_size > 5 * 1048576:
            raise ValueError("Signed APK exceeds the 5 MiB size limit")
        output.write_bytes(signed.read_bytes())
    print(f"Verified {output.name}; certificate SHA-256: {expected}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--build-tools", type=Path, required=True)
    args = parser.parse_args()
    try:
        sign_apk(args.source, args.output, args.build_tools)
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"APK signing failed: {error}\n")


if __name__ == "__main__":
    main()
