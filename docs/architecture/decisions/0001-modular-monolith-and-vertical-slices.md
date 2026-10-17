# ADR-0001: Modular monolith with vertical feature slices

- **Status:** Accepted
- **Context:** Nibleaf already deploys one API and one worker around a shared PostgreSQL schema. Source inspection shows many product capabilities but no verified workload/deployment requirement for independent microservices. MedBrains demonstrates useful feature-crate boundaries but has a much larger domain-specific graph.
- **Alternatives:** (1) microservices per capability; (2) global controller/service/repository layers; (3) one Rust application crate with folder-only slices; (4) modular monolith with Axum-composable feature crates.
- **Decision:** Use a modular monolith. Each initial feature crate owns typed HTTP input, validation, authorization invocation, application flow and direct SQL. `core` contains only stable cross-cutting concepts; `outbox` is a focused shared reliability primitive. API and worker are separate binaries.
- **Consequences:** Faster transactional evolution and a simple deployment topology; no distributed RPC/broker requirement. Cross-feature compile boundaries must remain acyclic. Features must not use `core` as a dumping ground.
- **Verification:** `rust/scripts/check-architecture.sh`; `cargo metadata`; slice tests; a review rule that a feature's SQL and use case remain in its crate.
