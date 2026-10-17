# Additive production container definitions for the Rust platform. Build from the
# repository root so Cargo workspace paths remain stable:
#   docker build --file rust/Dockerfile --target api --tag nibleaf-api:local .
#   docker build --file rust/Dockerfile --target worker --tag nibleaf-worker:local .
#
# The legacy root Dockerfile is intentionally untouched during the incremental
# migration. Its Node deployment continues to own every route not explicitly
# routed to this Rust slice through the same-origin compatibility adapter.

FROM rust:1.88-bookworm AS builder

# object_store's reviewed S3/TLS graph includes aws-lc-sys. Native CI/local
# release builds verify code, not image execution; provision its CMake toolchain
# explicitly rather than relying on an accidental base-image package.
RUN apt-get update \
    && apt-get install --no-install-recommends --yes cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*
ENV CARGO_BUILD_JOBS=1

WORKDIR /source/rust
COPY rust/ ./
RUN cargo build --locked --release --bin nibleaf-api --bin nibleaf-worker --bin search-ops

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 --shell /usr/sbin/nologin nibleaf

WORKDIR /app
ENV RUST_LOG=info
STOPSIGNAL SIGTERM
USER nibleaf

FROM runtime AS api
COPY --from=builder --chown=nibleaf:nibleaf /source/rust/target/release/nibleaf-api /app/nibleaf-api
EXPOSE 4311
ENTRYPOINT ["/app/nibleaf-api"]

FROM runtime AS worker
COPY --from=builder --chown=nibleaf:nibleaf /source/rust/target/release/nibleaf-worker /app/nibleaf-worker
EXPOSE 4312
ENTRYPOINT ["/app/nibleaf-worker"]

# Operator-only verifier/distributor. This target does not deploy/promote or
# acknowledge tenant erasure. Do not expose it as a public HTTP service.
FROM runtime AS tools
COPY --from=builder --chown=nibleaf:nibleaf /source/rust/target/release/search-ops /app/search-ops
ENTRYPOINT ["/app/search-ops"]
