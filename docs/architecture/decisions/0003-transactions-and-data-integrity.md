# ADR-0003: PostgreSQL/SQLx with explicit transactions and constraints

- **Status:** Accepted
- **Context:** Nibleaf uses PostgreSQL and Prisma migrations. Data invariants such as page path uniqueness and edit concurrency were partly application-level. The target requires inspectable SQL and strong final database enforcement.
- **Alternatives:** retain Prisma in Rust; use an ORM repository abstraction; SQLx direct queries; event sourcing.
- **Decision:** Use SQLx/PostgreSQL direct typed row mappings in feature slices. New schema changes are SQL migrations. Transactions are written at the use-case boundary. The first migration adds page path uniqueness, page revision, onboarding relation and active-publication uniqueness.
- **Consequences:** Queries stay visible and are easy to tune; each feature owns more explicit SQL. SQLx dynamic queries require real PostgreSQL migration/E2E tests until compile-time offline query metadata is introduced.
- **Verification:** fresh legacy-schema migration run on PostgreSQL 17; duplicate preflight in migration; page stale-write 409 integration test; constrained publication test.
