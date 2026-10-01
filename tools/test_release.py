#!/usr/bin/env python3
"""Release validation must fail before publishing mislabeled builds."""
from pathlib import Path
import tempfile
import unittest

from release_metadata import release_metadata
from sign_android import fingerprint


class ReleaseValidationTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "android/app").mkdir(parents=True)
        self.versions("1.2.3", "1.2.3", 12)

    def versions(self, rust, android, code):
        (self.root / "Cargo.toml").write_text(f'[workspace.package]\nversion = "{rust}"\n')
        (self.root / "android/app/build.gradle.kts").write_text(
            f'versionName = "{android}"\nversionCode = {code}\n')

    def test_stable_release(self):
        self.assertEqual(release_metadata(self.root, "v1.2.3"), {"version": "1.2.3", "prerelease": "false"})

    def test_prerelease(self):
        self.versions("1.2.3-rc.1", "1.2.3-rc.1", 13)
        self.assertEqual(release_metadata(self.root, "v1.2.3-rc.1")["prerelease"], "true")

    def test_malformed_tags(self):
        for tag in ("1.2.3", "v01.2.3", "v1.2", "v1.2.3\n", "v1.2.3-rc.01", "v1.2.3+build", "v1.2.3;echo x"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release_metadata(self.root, tag)

    def test_embedded_versions_must_both_match(self):
        for rust, android in (("1.2.4", "1.2.3"), ("1.2.3", "1.2.4")):
            self.versions(rust, android, 12)
            with self.subTest(rust=rust, android=android), self.assertRaises(ValueError):
                release_metadata(self.root, "v1.2.3")

    def test_invalid_android_version_code(self):
        for code in (0, -1, 2100000001):
            self.versions("1.2.3", "1.2.3", code)
            with self.subTest(code=code), self.assertRaises(ValueError):
                release_metadata(self.root, "v1.2.3")

    def test_certificate_fingerprint(self):
        self.assertEqual(fingerprint(":".join(["AB"] * 32)), "ab" * 32)
        for value in ("", "ab" * 31, "xz" * 32):
            with self.subTest(value=value), self.assertRaises(ValueError):
                fingerprint(value)


if __name__ == "__main__":
    unittest.main()
