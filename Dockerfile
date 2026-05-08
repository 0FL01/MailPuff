# syntax=docker/dockerfile:1

FROM rust:1.94-alpine AS builder
WORKDIR /src

RUN apk add --no-cache musl-dev

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked

FROM alpine:3.22
WORKDIR /app

RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S app && adduser -S app -G app

COPY --from=builder /src/target/release/mailpuff /app/mailpuff

USER app

ENTRYPOINT ["/app/mailpuff"]
