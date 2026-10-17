# ADR-0002: Shared tables with project-owned organization tenant scope

- **Status:** Accepted
- **Context:** Current Nibleaf creates a dedicated `Organization` for each `Project` and resolves project membership from `Member`. Existing database tables are already shared and most tenant-owned rows reference project IDs.
- **Alternatives:** schema-per-tenant; new workspace-many-project redesign; shared tables + explicit scope; shared tables + immediate RLS.
- **Decision:** Preserve the existing per-project organization as tenant boundary in shared PostgreSQL tables. Rust operations derive `ProjectScope` by joining the authenticated user, project and membership. Missing and unauthorized objects both produce `404`. No caller-supplied tenant identifier is authoritative. Phase one does not enable RLS while legacy Prisma and SQLx coexist.
- **Consequences:** No schema fan-out or tenant provisioning migration. Every feature must explicitly scope project/tenant ownership; missing scope is a defect. Membership mutations additionally serialize on a project-scoped advisory transaction lock, re-authorize the actor inside that transaction, and rely on a partial unique owner index as a final duplicate-owner backstop. RLS is deferred until transaction-local context and connection-pool leakage are tested.
- **Verification:** real-PostgreSQL cross-tenant negative tests; review all tenant-owned queries; composite project/tenant FK on onboarding records; role-bound and ownership-transfer E2E checks ending in exactly one owner.
