from __future__ import annotations

import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "release_manifest.py"
SPEC = importlib.util.spec_from_file_location("release_manifest", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
release_manifest = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_manifest)


def write_fixture_tree(directory: Path) -> Path:
    (directory / "LICENSE").write_text("MIT License\n", encoding="utf-8")
    (directory / "Cargo.toml").write_text(
        '[package]\nname = "opscodex"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )
    (directory / "Cargo.lock").write_text(
        """\
version = 4

[[package]]
name = "opscodex"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
""",
        encoding="utf-8",
    )
    web = directory / "web"
    web.mkdir()
    (web / "package-lock.json").write_text(
        json.dumps(
            {
                "name": "opscodex-web",
                "version": "0.1.0",
                "lockfileVersion": 3,
                "packages": {
                    "": {"name": "opscodex-web", "version": "0.1.0"},
                    "node_modules/react": {"version": "18.3.1"},
                },
            }
        ),
        encoding="utf-8",
    )
    binary = directory / "target" / "release" / "opscodex"
    binary.parent.mkdir(parents=True)
    binary.write_bytes(b"fake-binary")
    dist = directory / "web" / "dist"
    dist.mkdir()
    (dist / "index.html").write_text("<!doctype html>", encoding="utf-8")
    return directory


class ReleaseManifestTests(unittest.TestCase):
    def test_missing_license_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = write_fixture_tree(Path(raw))
            (root / "LICENSE").unlink()
            with self.assertRaises(release_manifest.ManifestError) as raised:
                release_manifest.write_manifest(root, root / "out")
            self.assertIn("LICENSE", str(raised.exception))

    def test_manifest_records_checksums_and_sbom_without_publishing(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = write_fixture_tree(Path(raw))
            out = root / "dist" / "release-dry-run"
            manifest = release_manifest.write_manifest(
                root,
                out,
                binary=root / "target" / "release" / "opscodex",
                web_dir=root / "web" / "dist",
            )
            self.assertEqual(manifest["name"], "opscodex")
            self.assertEqual(manifest["version"], "0.1.0")
            self.assertFalse(manifest["published"])
            self.assertIsNone(manifest["tag"])
            self.assertEqual(manifest["license"], "MIT")

            sbom = json.loads((out / "sbom.cdx.json").read_text(encoding="utf-8"))
            names = {item["name"] for item in sbom["components"]}
            self.assertIn("serde", names)
            self.assertIn("react", names)
            self.assertEqual(sbom["metadata"]["component"]["licenses"][0]["license"]["id"], "MIT")

            checksums = (out / "SHA256SUMS").read_text(encoding="utf-8")
            expected = hashlib.sha256(b"fake-binary").hexdigest()
            self.assertIn(f"{expected}  target/release/opscodex", checksums)
            self.assertIn("web/dist/index.html", checksums)

            release_manifest.verify_sha256sums(root, out / "SHA256SUMS")

    def test_verify_detects_tampered_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = write_fixture_tree(Path(raw))
            out = root / "out"
            release_manifest.write_manifest(
                root,
                out,
                binary=root / "target" / "release" / "opscodex",
            )
            (root / "target" / "release" / "opscodex").write_bytes(b"tampered")
            with self.assertRaises(release_manifest.ManifestError) as raised:
                release_manifest.verify_sha256sums(root, out / "SHA256SUMS")
            self.assertIn("checksum mismatch", str(raised.exception))

    def test_missing_binary_fails(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = write_fixture_tree(Path(raw))
            with self.assertRaises(release_manifest.ManifestError):
                release_manifest.write_manifest(
                    root,
                    root / "out",
                    binary=root / "target" / "release" / "missing",
                )

    def test_cli_verify_returns_zero(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = write_fixture_tree(Path(raw))
            out = root / "out"
            release_manifest.write_manifest(
                root,
                out,
                binary=root / "target" / "release" / "opscodex",
            )
            self.assertEqual(
                release_manifest.main(
                    ["--root", str(root), "--verify", str(out / "SHA256SUMS")]
                ),
                0,
            )


if __name__ == "__main__":
    unittest.main()
