#!/usr/bin/env python3
"""Offline integrity/pin check for the reviewed native-search dependency patch.

The versioned provenance inventory was created from the SHA-256-verified
published crate archive. CI checks every extracted file and the single allowed
manifest replacement; it does not trust a path dependency merely because Cargo
cannot validate its registry checksum. This is a review/integrity gate, not a
claim that a repository administrator cannot change the gate itself.
"""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[1]
ARCHIVE_SHA256 = "861facfabd71044968f364837f9a083b56464ba5a59079f88706ee5c451ca069"
OLD_REQUIREMENT = '[dependencies.lru]\nversion = "0.16.3"'
NEW_REQUIREMENT = '[dependencies.lru]\nversion = "=0.18.5"'


def sha256(content):
    return hashlib.sha256(content).hexdigest()


def vendor_violations(directory, provenance):
    """Check a reviewed upstream inventory with exactly one dependency edit."""
    violations = []
    expected = provenance["upstream_file_sha256"]
    actual = set()
    for path in directory.rglob("*"):
        if path.is_symlink():
            violations.append(f"unexpected symlink: {path.relative_to(directory)}")
        elif path.is_file():
            actual.add(str(path.relative_to(directory)))
    if actual != set(expected):
        violations.append("vendored file inventory differs from the reviewed crate archive")
    original = provenance["upstream_cargo_toml"]
    if sha256(original.encode()) != expected.get("Cargo.toml"):
        violations.append("upstream manifest does not match its reviewed checksum")
    if original.count(OLD_REQUIREMENT) != 1:
        violations.append("upstream lru requirement is not the reviewed single occurrence")
    patched = original.replace(OLD_REQUIREMENT, NEW_REQUIREMENT, 1)
    for name, checksum in expected.items():
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            violations.append("unsafe path in provenance inventory")
            continue
        path = directory / relative
        if not path.is_file() or path.is_symlink():
            continue
        if name == "Cargo.toml":
            if path.read_bytes() != patched.encode():
                violations.append("Tantivy manifest differs from the sole reviewed lru patch")
        elif sha256(path.read_bytes()) != checksum:
            violations.append(f"upstream source modified: {name}")
    return violations


def check():
    directory = ROOT / "vendor/tantivy-0.26.2"
    provenance = json.loads((ROOT / "vendor/tantivy-0.26.2.provenance.json").read_text())
    expected_identity = {
        "package": "tantivy",
        "version": "0.26.2",
        "source_url": "https://static.crates.io/crates/tantivy/tantivy-0.26.2.crate",
        "source_archive_sha256": ARCHIVE_SHA256,
        "upstream_git_commit": "72d1ef9a6468aa68bbc69dcc80cdf60aaf64364d",
        "allowed_patch": {"file": "Cargo.toml", "old": OLD_REQUIREMENT, "new": NEW_REQUIREMENT},
    }
    violations = [
        f"unexpected provenance identity: {key}"
        for key, value in expected_identity.items()
        if provenance.get(key) != value
    ]
    if len(provenance["upstream_file_sha256"]) != 374:
        violations.append("unexpected upstream archive file count")
    violations.extend(vendor_violations(directory, provenance))
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    if manifest["workspace"]["resolver"] != "3":
        violations.append("Rust-version-aware Cargo resolver 3 is required")
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version=1", "--manifest-path", str(ROOT / "Cargo.toml")],
        text=True,
    ))
    packages = metadata["packages"]
    expected_versions = {
        "tantivy": "0.26.2", "lru": "0.18.5", "lindera": "6.2.0",
        "lindera-dictionary": "6.2.0", "lindera-ipadic": "6.2.0", "lindera-cc-cedict": "6.2.0",
    }
    for name, version in expected_versions.items():
        found = [p for p in packages if p["name"] == name]
        if len(found) != 1 or found[0]["version"] != version:
            violations.append(f"runtime pin differs: {name} must resolve exactly once to {version}")
        elif name == "tantivy":
            if found[0]["source"] is not None or Path(found[0]["manifest_path"]).resolve() != directory / "Cargo.toml":
                violations.append("Tantivy must use the reviewed local manifest-only patch")
            if found[0]["id"] in metadata["workspace_members"]:
                violations.append("third-party Tantivy must not become a Nibleaf workspace member")
        elif not (found[0]["source"] or "").startswith("registry+"):
            violations.append(f"{name} must retain registry checksum verification")
    forbidden = {"encoding", "unsafe-libyaml", "serde_yaml", "serde_yaml_ng", "lindera-tantivy", "lindera-analysis"}
    baseline = tuple(int(x) for x in manifest["workspace"]["package"]["rust-version"].split("."))
    for package in packages:
        name = package["name"]
        if name in forbidden:
            violations.append(f"removed dependency became reachable: {name}")
        if name.startswith("lindera-") and name not in expected_versions:
            violations.append(f"unreviewed Lindera package/feature: {name}")
        required = package.get("rust_version")
        if required and tuple(int(x) for x in required.split(".")[:2]) > baseline[:2]:
            violations.append(f"declared MSRV exceeds workspace baseline: {name} requires {required}")
    lindera_ids = {p["id"] for p in packages if p["name"] == "lindera"}
    for node in metadata["resolve"]["nodes"]:
        if node["id"] in lindera_ids and set(node["features"]) != {
            "embed-ipadic", "embed-cc-cedict", "lindera-ipadic", "lindera-cc-cedict",
        }:
            violations.append("Lindera must use only the two reviewed embedded dictionaries")
    if violations:
        raise SystemExit("\n".join(f"search dependency check: {item}" for item in violations))
    print("search dependency check: 374 upstream files, sole lru patch, exact runtime/dictionary pins and declared MSRVs verified")


class IntegrityRegressionTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix="nibleaf-search-provenance-")
        self.addCleanup(self.scratch.cleanup)
        self.directory = Path(self.scratch.name)
        (self.directory / "src").mkdir()
        self.original = OLD_REQUIREMENT + "\n"
        (self.directory / "Cargo.toml").write_text(NEW_REQUIREMENT + "\n")
        (self.directory / "src/lib.rs").write_text("// reviewed upstream source\n")
        self.provenance = {
            "upstream_cargo_toml": self.original,
            "upstream_file_sha256": {
                "Cargo.toml": sha256(self.original.encode()),
                "src/lib.rs": sha256((self.directory / "src/lib.rs").read_bytes()),
            },
        }

    def test_reviewed_patch_passes(self):
        self.assertEqual(vendor_violations(self.directory, self.provenance), [])

    def test_source_change_fails(self):
        (self.directory / "src/lib.rs").write_text("// unreviewed change\n")
        self.assertTrue(vendor_violations(self.directory, self.provenance))

    def test_extra_file_fails(self):
        (self.directory / "src/unreviewed.rs").write_text("// extra source\n")
        self.assertTrue(vendor_violations(self.directory, self.provenance))

    def test_unpatched_dependency_fails(self):
        (self.directory / "Cargo.toml").write_text(self.original)
        self.assertTrue(vendor_violations(self.directory, self.provenance))

    def test_unrelated_manifest_edit_fails(self):
        with (self.directory / "Cargo.toml").open("a") as file:
            file.write('unexpected = "change"\n')
        self.assertTrue(vendor_violations(self.directory, self.provenance))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="also run five negative/positive integrity regression cases")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(IntegrityRegressionTests))
        if not result.wasSuccessful():
            sys.exit(1)
    check()
