FROM lukemathwalker/cargo-chef:latest-rust-1 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies — this is the caching Docker layer.
ENV SQLX_OFFLINE=1
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin paperclips-gallery

FROM debian:stable-slim AS final
WORKDIR /app

RUN apt-get update && apt-get install -y \
  ca-certificates \
  && rm -rf /var/lib/apt/lists/* \
  && update-ca-certificates

COPY --from=builder /app/target/release/paperclips-gallery .
# The editorial seed, so `paperclips-gallery import` can run in-container
# (PCG_METADATA must point at a mounted/downloaded metadata.jsonl).
COPY catalog.json .

EXPOSE 4601

ENTRYPOINT ["./paperclips-gallery"]
