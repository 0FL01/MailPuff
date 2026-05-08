# Project: MailPuff

MailPuff polls a mail source, publishes each new email body into a temporary in-memory HTML viewer, and sends a Telegram message with buttons to open the HTML or mark the mail as read.

Tech stack target: Rust `2024` (`rustc >= 1.94`), Docker-only runtime, `tokio`, `tracing`, typed env config, in-memory state only. Go code is still present as behavioral reference during migration.

## Workspace Overview
- `src/main.rs` - thin Rust entrypoint: config load, tracing init, top-level run.
- `src/config.rs` - typed env parsing, defaults, fail-fast validation, secret redaction.
- `src/mail_source` - provider-neutral mail source boundary; IMAP is the first backend, Proton custom is reserved.
- `cmd/mailpuff/main.go` - legacy Go reference for orchestration behavior during migration.
- `pkg/config` - environment parsing, defaults, required variable validation.
- `pkg/imap` - IMAP connect/select/search/fetch/mark-seen wrapper.
- `pkg/email` - email summary extraction; prefers HTML, falls back to escaped `text/plain` inside `<pre>`.
- `pkg/viewer` - in-memory page store, token authorization, sanitization, TTL cleanup, max-view deletion, `/view` and `/mark_read` endpoints.
- `pkg/telegram` - Telegram message formatting and inline keyboard creation.

## Where To Look
- `README.md` - user-facing behavior, env vars, Docker run/compose examples, limitations.
- `.env.example` - safe env template; do not read or commit real `.env` values.
- `Dockerfile` - canonical build path: Rust release binary copied into Alpine runtime.
- `docker-compose.yml` - local compose service; currently binds container `8080` to `127.0.0.1:82`.
- `Cargo.toml` - Rust 2024 binary crate `mailpuff`; keep `Cargo.lock` committed.
- `go.mod` - legacy Go module retained as reference until migration parity.

## Architectural Invariants
- Runtime is intended to be Docker-only; prefer Docker/Compose instructions for operation.
- Viewer pages are stored only in process memory. Container restart invalidates all existing links.
- Message deduplication must remain process-local; after restart, old unseen messages can be processed again.
- Never bypass `pkg/viewer` sanitization when storing or serving email HTML.
- Viewer URLs must include both `id` and `token`; do not log tokens, and keep page IDs masked in logs.
- `IMAP_TLS=false` is legacy compatibility mode; plaintext IMAP must not be enabled silently. Use `IMAP_ACCEPT_INVALID_CERTS=true` only with explicit warning.
- Telegram messages are not deleted when viewer pages expire; only the in-memory page is removed.
- `.env` is ignored and may contain secrets. Avoid reading it unless explicitly required.

## Key Subsystems

### IMAP Polling
- Rust migration should route polling through `src/mail_source::MailSource`, not directly through IMAP-specific state.
- Legacy main loop lives in `cmd/mailpuff/main.go`; it reconnects each poll, searches `UNSEEN`, fetches emails, skips already processed UIDs, and sleeps `IMAP_POLL_INTERVAL`.
- IMAP operations are isolated in `pkg/imap/imap.go`; use those wrappers instead of calling the library directly from new code.
- `IMAP_FORCE_RECONNECT` is loaded in config but is not currently used by the polling logic.

### Email Parsing
- `pkg/email/email.go` builds `Summary` with subject, sender fields, date, and HTML body.
- If HTML is missing, `text/plain` is escaped and wrapped in `<pre>`; emails with no HTML/text body are skipped by `cmd/mailpuff/main.go`.

### Viewer
- `pkg/viewer/viewer.go` owns page lifecycle: `CreatePage`, `ViewWithReason`, `Authorize`, TTL deletion, and max-view deletion.
- Sanitization uses a customized `bluemonday.UGCPolicy`; scripts and event handlers must remain blocked.
- `/view` increments views and may trigger first-view callbacks; `/mark_read` authorizes without incrementing views.

### Telegram
- `pkg/telegram/telegram.go` sends one message with `Open html` URL button and `Mark as read` callback button.
- Mark-read callback state is in memory in `cmd/mailpuff/main.go`; expired/missing callback keys should be treated as invalid links.
- When mail becomes read, keyboard should be edited to remove `Mark as read` and keep only `Open html`.

## Development Practices
- Build image: `docker build -t mailpuff:latest .`
- Run with compose: `docker compose up -d`; logs: `docker compose logs -f`.
- Rust checks: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- Legacy Go reference checks, when touched: `gofmt -w cmd pkg`, `go test ./...`, `go vet ./...`.
- There is currently no Makefile or CI workflow in the repo.

## Commit Style
- Prefer concise conventional commits: `feat(scope): description`, `fix(scope): description`, `chore(scope): description`, `docs(scope): description`, `refactor(scope): description`, `test(scope): description`.
- Keep bodies factual and focused on behavior changes, risks, and validation commands.

## Where To Find Details
- `README.md` - env variables, viewer behavior, security notes, known limitations.
- `src/config.rs` - Rust env contract and validation behavior.
- `src/mail_source` - provider-neutral source model for IMAP and future custom providers.
- `cmd/mailpuff/main.go` - legacy end-to-end data flow and Telegram/IMAP/viewer integration.
- `pkg/viewer/viewer.go` - HTTP routes, token checks, TTL/max-view logic.
- `pkg/config/config.go` - required env vars and defaults.
