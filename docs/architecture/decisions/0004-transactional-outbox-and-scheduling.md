# ADR-0004: PostgreSQL transactional outbox now; scheduling decision deferred

- **Status:** Accepted for outbox; deferred for generic scheduler
- **Context:** Existing BullMQ jobs are operationally useful but Redis enqueue cannot atomically share a PostgreSQL transaction. Nibleaf already has export schedules and recurring analytics but their complete compatibility requirements need a separate porting investigation.
- **Alternatives:** keep BullMQ enqueue as source of truth; introduce Kafka/RabbitMQ; build a PostgreSQL outbox; add a generic scheduler now.
- **Decision:** Add `outbox_event` and append an event in the transaction whenever committed project/publication state requires async processing. Claims use `FOR UPDATE SKIP LOCKED`, tokenized leases, bounded exponential jitter, attempts and `dead_letter`. Do not claim exactly-once delivery. Do not add an unused generic scheduler yet.
- **Consequences:** API commits are independent from worker/provider availability and duplicate delivery must be safe. Operators need dead-letter replay and queue-age monitoring. Export schedule migration will evaluate an established Rust library versus a focused PostgreSQL design in a follow-up ADR.
- **Verification:** expired-lease recovery was run against PostgreSQL; events were delivered once after reclaim; publication consumer is idempotent when already READY.
