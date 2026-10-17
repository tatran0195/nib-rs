# ADR-0005: Legacy-session bridge and versioned Rust API contract

- **Status:** Accepted as transitional design
- **Context:** Nibleaf's Better Auth session/OAuth/OTP behavior is already exposed to the dashboard. Replacing it without compatibility tests would be an account-access risk. Existing Hono errors use a custom `{error:{code,message}}` shape.
- **Alternatives:** reimplement auth before all features; accept unauthenticated migration endpoints; create incompatible JWTs; query legacy sessions while moving feature routes; retain unversioned response contract.
- **Decision:** Rust reads the existing opaque session table and fresh user status for authenticated requests. It accepts bearer tokens or explicit `nibleaf_session` migration cookie. Rust routes are `/v1`, typed, same-origin by default, and return RFC 9457 Problem Details. Full login/OTP/OAuth/cookie issuance remains a dedicated deferred security slice.
- **Consequences:** The dashboard needs an adapter/proxy during incremental migration. There is no claim that Better Auth has been replaced. API compatibility is deliberate rather than accidental.
- **Verification:** live legacy-session authorization, unauthenticated 401 Problem Details, and cross-tenant 404 were exercised against PostgreSQL.
