# syntax=docker/dockerfile:1

FROM golang:1.25-alpine AS builder
ENV GOTOOLCHAIN=auto
WORKDIR /src

# Install git and CA certs
RUN apk add --no-cache git ca-certificates

# Cache dependencies
COPY go.mod go.sum ./
RUN go mod download

# Copy source
COPY . .

# Refresh dependencies to regenerate go.sum after COPY
RUN go mod download
RUN go mod tidy

# Build static binary
RUN CGO_ENABLED=0 GOOS=linux go build -ldflags="-s -w" -o /out/mailpuff ./cmd/mailpuff

FROM alpine:3.22
WORKDIR /app

# CA certificates and tzdata for correct HTTPS and timezone handling
RUN apk add --no-cache ca-certificates tzdata \
    && addgroup -S app && adduser -S app -G app

COPY --from=builder /out/mailpuff /app/mailpuff

USER app

ENTRYPOINT ["/app/mailpuff"]