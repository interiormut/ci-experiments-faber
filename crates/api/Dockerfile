# syntax=docker/dockerfile:1

FROM rust:bookworm AS chef

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends libpq-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked

FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS cacher

COPY --from=planner /app/recipe.json recipe.json

RUN --mount=type=cache,id=faber-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=faber-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=faber-api-target,target=/app/target,sharing=locked \
    cargo chef cook --release --locked --recipe-path recipe.json

FROM cacher AS builder

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN --mount=type=cache,id=faber-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=faber-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=faber-api-target,target=/app/target,sharing=locked \
    cargo build --release --locked -p api \
    && mkdir -p /out \
    && cp target/release/api /out/faber-api

FROM debian:bookworm-slim AS runner

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libpq5 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /usr/sbin/nologin faber

WORKDIR /app

ENV API_PORT=3001

COPY --from=builder /out/faber-api /usr/local/bin/faber-api

USER faber

EXPOSE 3001

CMD ["faber-api"]
