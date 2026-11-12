# Builder: rust 1.96 (matches the toolchain that produced Cargo.lock; >=1.85 is
# required because transitive dep icu_collections needs edition2024) + the C
# toolchain ring/rustls need.
FROM rust:1.96-slim AS builder
WORKDIR /app
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential \
 && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# Runtime: no compiler, no cargo, no target/ -> only TLS roots + the binary.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/uzawa-reisa /usr/local/bin/uzawa-reisa
RUN useradd --system --uid 10001 bot
USER bot
CMD ["/usr/local/bin/uzawa-reisa"]