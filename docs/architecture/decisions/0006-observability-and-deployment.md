# ADR-0006: Separate API/worker binaries, controlled migrations, structured telemetry

- **Status:** Accepted
- **Context:** Existing compose deployment separates server and worker and has health endpoints. The Rust target needs reliable lifecycle ownership without implicit production schema changes.
- **Alternatives:** one combined API/worker process; migration at app startup; detached task workers; independent API/worker with one-shot migrations.
- **Decision:** Deploy API and worker independently. Run migrations as a one-shot pre-deploy job. API and worker fail readiness on absent Rust platform schema, expose liveness/readiness separately, use structured `tracing` JSON, and respond to shutdown signals. Worker leases make unacknowledged work recoverable.
- **Consequences:** Deployment needs explicit ordering and an operator migration role. Metrics exporter and production dashboards are still required before full cutover.
- **Verification:** both binaries started on local PostgreSQL 17; health endpoints returned ready; worker reclaimed an expired lease. CI workflow includes build/lint/test/migration/E2E stages.
