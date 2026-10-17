#!/usr/bin/env bash
# Provision an explicitly disposable local PostgreSQL database for the Rust
# integration harness. It never drops data; the harness itself refuses a URL
# whose database name lacks `nibleaf_rust_test` before recreating public.
set -euo pipefail

postgres_major=${NIBLEAF_POSTGRES_MAJOR:-17}
database=${NIBLEAF_TEST_DATABASE_NAME:-nibleaf_rust_test}
role=${NIBLEAF_TEST_DATABASE_ROLE:-nibleaf_test}
password=${NIBLEAF_TEST_DATABASE_PASSWORD:-nibleaf_test_password}

if [[ "$database" != *nibleaf_rust_test* ]]; then
  echo 'Refusing a non-disposable test database name; it must contain nibleaf_rust_test.' >&2
  exit 2
fi
if ! command -v psql >/dev/null || ! command -v pg_ctlcluster >/dev/null; then
  cat >&2 <<'EOF'
PostgreSQL client/server utilities are missing. Install PostgreSQL 17 using
system package management, then rerun this local-only setup script.
EOF
  exit 2
fi

sudo pg_ctlcluster "$postgres_major" main start
sudo -u postgres psql -v ON_ERROR_STOP=1 \
  --set=role_name="$role" --set=role_password="$password" --set=database_name="$database" <<'SQL'
SELECT format(
  'CREATE ROLE %I LOGIN PASSWORD %L', :'role_name', :'role_password'
)
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'role_name')
\gexec
SELECT format('CREATE DATABASE %I OWNER %I', :'database_name', :'role_name')
WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = :'database_name')
\gexec
SQL

printf 'export NIBLEAF_TEST_DATABASE_URL=%q\n' \
  "postgresql://${role}:${password}@127.0.0.1:5432/${database}"
