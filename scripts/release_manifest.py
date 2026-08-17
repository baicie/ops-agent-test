#!/usr/bin/env python3
"""Build a local release manifest: checksums, CycloneDX SBOM, and license check.

This is a dry-run helper. It does not create git tags, GitHub Releases, or
upload artifacts. v1.0.0 is not published until the phase gates pass.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import tomllib
from pathlib import Path
from typing import Any

CHUNK_SIZE = 1024 * 1024


class ManifestError(Exception):
    """User-facing failure while assembling a release dry-run."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(CHUNK_SIZE)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def read_package_version(cargo_toml: Path) -> tuple[str, str]:
    data = tomllib.loads(cargo_toml.read_text(encoding="utf-8"))
    package = data.get("package") or {}
    name = str(package.get("name") or "").strip()
    version = str(package.get("version") or "").strip()
    if not name or not version:
        raise ManifestError(f"{cargo_toml} is missing package name or version")
    return name, version


def cargo_components(lock_text: str) -> list[dict[str, Any]]:
    data = tomllib.loads(lock_text)
    components: list[dict[str, str]] = []
    seen: set[str] = set()
    for package in data.get("package") or []:
        name = str(package.get("name") or "").strip()
        version = str(package.get("version") or "").strip()
        if not name or not version:
            continue
        bom_ref = f"cargo:{name}@{version}"
        if bom_ref in seen:
            continue
        seen.add(bom_ref)
        component = {
            "type": "library",
            "name": name,
            "version": version,
            "purl": f"pkg:cargo/{name}@{version}",
            "bom-ref": bom_ref,
        }
        checksum = str(package.get("checksum") or "").strip()
        if checksum:
            component["hashes"] = [{"alg": "SHA-256", "content": checksum}]
        components.append(component)
    return components


def npm_components(lock: dict[str, Any]) -> list[dict[str, Any]]:
    components: list[dict[str, str]] = []
    seen: set[str] = set()
    packages = lock.get("packages") or {}
    if not isinstance(packages, dict):
        raise ManifestError("package-lock.json packages must be an object")
    for path, meta in packages.items():
        if not isinstance(meta, dict):
            continue
        version = str(meta.get("version") or "").strip()
        if not version:
            continue
        name = str(meta.get("name") or "").strip()
        if not name:
            key = str(path)
            if key in {"", "."}:
                name = str(lock.get("name") or "opscodex-web")
            else:
                name = key.rsplit("node_modules/", 1)[-1]
        if not name:
            continue
        bom_ref = f"npm:{name}@{version}"
        if bom_ref in seen:
            continue
        seen.add(bom_ref)
        components.append(
            {
                "type": "library",
                "name": name,
                "version": version,
                "purl": f"pkg:npm/{name}@{version}",
                "bom-ref": bom_ref,
            }
        )
    return components


def collect_artifacts(root: Path, binary: Path | None, web_dir: Path | None) -> list[dict[str, Any]]:
    artifacts: list[dict[str, Any]] = []
    paths: list[Path] = []
    if binary is not None:
        paths.append(binary)
    if web_dir is not None:
        if not web_dir.is_dir():
            raise ManifestError(f"web directory {web_dir} is not a directory")
        for child in sorted(web_dir.rglob("*")):
            if child.is_file():
                paths.append(child)
    for path in paths:
        if not path.is_file():
            raise ManifestError(f"release artifact {path} does not exist")
        relative = path.resolve().relative_to(root.resolve()).as_posix()
        artifacts.append(
            {
                "path": relative,
                "sha256": sha256_file(path),
                "bytes": path.stat().st_size,
            }
        )
    return artifacts


def build_sbom(
    name: str,
    version: str,
    cargo: list[dict[str, Any]],
    npm: list[dict[str, Any]],
) -> dict[str, Any]:
    return {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "name": name,
                "version": version,
                "licenses": [{"license": {"id": "MIT"}}],
            }
        },
        "components": cargo + npm,
    }


def write_sha256sums(path: Path, artifacts: list[dict[str, Any]]) -> None:
    lines = [f"{item['sha256']}  {item['path']}\n" for item in artifacts]
    path.write_text("".join(lines), encoding="utf-8")


def verify_sha256sums(root: Path, sums_path: Path) -> None:
    text = sums_path.read_text(encoding="utf-8")
    if not text.strip():
        raise ManifestError(f"{sums_path} is empty")
    for line_number, raw in enumerate(text.splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) != 2:
            raise ManifestError(f"{sums_path}:{line_number} is not `hash  path`")
        expected, relative = parts
        target = root / relative
        if not target.is_file():
            raise ManifestError(f"missing artifact {relative}")
        actual = sha256_file(target)
        if actual != expected:
            raise ManifestError(f"checksum mismatch for {relative}")


def write_manifest(
    root: Path,
    out: Path,
    binary: Path | None = None,
    web_dir: Path | None = None,
) -> dict[str, Any]:
    root = root.resolve()
    license_file = root / "LICENSE"
    cargo_toml = root / "Cargo.toml"
    cargo_lock = root / "Cargo.lock"
    package_lock = root / "web" / "package-lock.json"
    for required in (license_file, cargo_toml, cargo_lock, package_lock):
        if not required.is_file():
            raise ManifestError(f"required file {required} is missing")

    name, version = read_package_version(cargo_toml)
    cargo = cargo_components(cargo_lock.read_text(encoding="utf-8"))
    npm = npm_components(json.loads(package_lock.read_text(encoding="utf-8")))
    if not cargo:
        raise ManifestError("Cargo.lock did not contain any packages")
    artifacts = collect_artifacts(root, binary, web_dir)

    out.mkdir(parents=True, exist_ok=True)
    sbom_name = "sbom.cdx.json"
    sums_name = "SHA256SUMS"
    manifest_name = "manifest.json"
    sbom = build_sbom(name, version, cargo, npm)
    (out / sbom_name).write_text(json.dumps(sbom, indent=2) + "\n", encoding="utf-8")
    write_sha256sums(out / sums_name, artifacts)
    manifest = {
        "name": name,
        "version": version,
        "published": False,
        "tag": None,
        "license": "MIT",
        "license_file": "LICENSE",
        "sbom": sbom_name,
        "checksums": sums_name,
        "component_counts": {"cargo": len(cargo), "npm": len(npm)},
        "artifacts": artifacts,
    }
    (out / manifest_name).write_text(
        json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
    )
    return manifest


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--out", type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--web-dir", type=Path)
    parser.add_argument(
        "--verify",
        type=Path,
        help="Verify an existing SHA256SUMS file against --root and exit",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.verify is not None:
            verify_sha256sums(args.root, args.verify)
            print(f"verified {args.verify}")
            return 0
        if args.out is None:
            raise ManifestError("--out is required unless --verify is set")
        manifest = write_manifest(
            args.root,
            args.out,
            binary=args.binary,
            web_dir=args.web_dir,
        )
        print(
            "release dry-run "
            f"{manifest['name']} {manifest['version']} "
            f"cargo={manifest['component_counts']['cargo']} "
            f"npm={manifest['component_counts']['npm']} "
            f"artifacts={len(manifest['artifacts'])} "
            f"published={manifest['published']}"
        )
        return 0
    except ManifestError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
