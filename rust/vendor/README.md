# Reviewed third-party dependency patch

`tantivy-0.26.2/` is the published MIT-licensed Tantivy 0.26.2 archive. Its
upstream source, license, version and index-format constants are unchanged.
Only the normalized `Cargo.toml` LRU requirement is changed:

```diff
 [dependencies.lru]
-version = "0.16.3"
+version = "=0.18.5"
```

The release's older requirement does not satisfy `RUSTSEC-2026-0253`; upstream
LRU 0.18.5 satisfies both that advisory and `RUSTSEC-2026-0002`. This is not an
advisory ignore or a version relabeling of old LRU code.

## Provenance and verification

- Published archive: <https://static.crates.io/crates/tantivy/tantivy-0.26.2.crate>
- Archive SHA-256: `861facfabd71044968f364837f9a083b56464ba5a59079f88706ee5c451ca069`
- Recorded upstream commit: `72d1ef9a6468aa68bbc69dcc80cdf60aaf64364d`
- `tantivy-0.26.2.provenance.json` records the original manifest and every
  upstream file's SHA-256. It was generated from the verified archive, not from
  a modified engine checkout.
- The archive's original `Cargo.toml.orig` and `Cargo.lock` are retained as
  upstream evidence. **The root `rust/Cargo.lock` is the actual build graph**;
  a copied upstream lockfile is not a second application dependency graph.

From `rust/`, run:

```bash
python3 scripts/check-search-dependencies.py --self-test
bash scripts/check-inactive-rsa.sh
cargo audit --ignore RUSTSEC-2023-0071 --deny warnings
```

The first command verifies all 374 upstream files, rejects any extra source or
unrelated edit, verifies the one allowed manifest replacement and the resolved
engine/analyzer/dictionary pins, and runs five guard regression tests. The
other commands keep the pre-existing RSA exception inactive and deny every
other advisory warning. None of these checks proves general memory safety.

## Removal condition

Replace this patch when a compatible **official Tantivy release** includes a
patched LRU requirement. Remove the source/provenance/path override together,
review exact generation-runtime compatibility, update the dependency guard,
and rerun locked source, Rust 1.88, audit, cache-eviction and real PostgreSQL
API/worker tests. Do not make a source edit inside this directory to work around
a test, and do not bypass the guard to refresh a dependency.

Rationale, native Lindera error handling, migration consequences and executed
evidence: [ADR-0014](../../docs/architecture/decisions/0014-search-runtime-dependency-remediation.md)
and [runtime migration](../../docs/migration/native-search-runtime-upgrade.md).
