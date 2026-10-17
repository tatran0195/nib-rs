#!/usr/bin/env bash
# Enforce crate-level boundaries using Cargo metadata rather than source-text
# matching. Run from any directory.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
metadata_file=$(mktemp)
trap 'rm -f "$metadata_file"' EXIT
cargo metadata --locked --format-version=1 --manifest-path "$root/Cargo.toml" > "$metadata_file"

python3 - "$metadata_file" <<'PY'
import json
import sys

metadata = json.load(open(sys.argv[1], encoding='utf-8'))
packages = {package['name']: package for package in metadata['packages']}
workspace = set(metadata['workspace_members'])
by_id = {package['id']: package['name'] for package in metadata['packages']}
local = {
    package['name']
    for package in metadata['packages']
    if package['id'] in workspace and package['name'].startswith('nibleaf-')
}

def local_dependencies(name):
    package = packages[name]
    names = set()
    for dependency in package['dependencies']:
        if dependency['name'] in local:
            names.add(dependency['name'])
    return names

violations = []
if local_dependencies('nibleaf-core'):
    violations.append(f"nibleaf-core must not depend on local crates: {sorted(local_dependencies('nibleaf-core'))}")
if local_dependencies('nibleaf-outbox') - {'nibleaf-core'}:
    violations.append(f"nibleaf-outbox may depend only on core: {sorted(local_dependencies('nibleaf-outbox'))}")
for feature in ('nibleaf-projects', 'nibleaf-membership', 'nibleaf-content', 'nibleaf-publishing', 'nibleaf-search'): 
    forbidden = local_dependencies(feature) - {'nibleaf-core', 'nibleaf-outbox'}
    if forbidden:
        violations.append(f"{feature} depends on another feature/app: {sorted(forbidden)}")
for app in ('nibleaf-api', 'nibleaf-worker'):
    deps = local_dependencies(app)
    if any(dep.startswith('nibleaf-api') or dep.startswith('nibleaf-worker') for dep in deps):
        violations.append(f"{app} may not depend on an app crate: {sorted(deps)}")

if violations:
    print('\n'.join(f'architecture check: {item}' for item in violations), file=sys.stderr)
    raise SystemExit(1)
print('architecture check: crate dependency graph satisfies declared boundaries')
PY
