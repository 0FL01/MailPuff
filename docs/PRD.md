**TL;DR:** Ниже PRD для полной миграции MailPuff на Rust 2024 Edition: поведение сохраняем, состояние держим строго в ОЗУ, персистентные хранилища запрещены. Я также заложил исправления текущих слабых мест Go-версии: очистка callback-map при TTL, явная TLS-семантика, нормальный shutdown, тестируемая in-memory модель.

# PRD: Полная миграция MailPuff на Rust 2024 Edition

## 1. Назначение продукта

MailPuff — Docker-only сервис, который опрашивает IMAP-почту, находит новые непрочитанные письма, публикует HTML письма во временном in-memory viewer и отправляет Telegram-сообщение с кнопками:

* `Open html` — открыть временную HTML-страницу.
* `Mark as read` — пометить письмо прочитанным через IMAP.

Ключевой инвариант: **никакого персистентного состояния**. Все страницы, токены, callback-ключи, UID-дедупликация и связи Telegram-сообщений хранятся только в памяти процесса. После рестарта контейнера все ссылки становятся недействительными, а старые unseen-письма могут быть обработаны повторно.

## 2. Цели миграции

Основная цель — полностью заменить Go-код на Rust-приложение с тем же продуктовым поведением, но с более строгой типизацией, контролем ошибок, безопасной HTML-обработкой и предсказуемой async-архитектурой.

Rust-проект должен использовать `edition = "2024"`. Rust 2024 официально доступен начиная с Rust release version `1.85.0`, но для проекта целевая минимальная версия компилятора должна быть поднята до `rustc >= 1.94.0`. ([doc.rust-lang.org][1])

Целевые цели:

* Полная замена Go entrypoint, пакетов и Docker build chain.
* Сохранение Docker-only runtime.
* Сохранение текущих env-переменных, где это разумно.
* Хранение runtime-состояния только в RAM.
* Улучшение безопасности viewer: санитизация, CSP, не логировать токены.
* Улучшение observability: structured logs через `tracing`.
* Покрытие тестами core-логики: config, viewer, token auth, TTL, max views, mark-read flow.
* Устранение неоднозначности `IMAP_TLS=false`.

## 3. Не-цели

В рамках миграции **не делать**:

* Базу данных, Redis, SQLite, файловый cache, disk-backed queue.
* Персистентную дедупликацию UID.
* Web UI для списка писем.
* Хранение вложений.
* OAuth2 для Gmail.
* Webhook-mode для Telegram.
* Миграцию старых viewer-ссылок из Go-версии.
* Автоудаление Telegram-сообщений при истечении TTL страницы, если это не будет отдельным флагом.

## 4. Текущее поведение, которое надо перенести

По присланному Go-коду сервис делает следующее:

* Загружает конфиг из env.
* Создаёт Telegram Bot API client.
* Создаёт in-memory viewer store.
* Запускает HTTP server с `/view` и `/mark_read`.
* Запускает Telegram updates loop для callback-кнопок.
* В бесконечном цикле:

  * подключается к IMAP;
  * выбирает mailbox;
  * ищет `UNSEEN`;
  * fetch-ит письма по UID;
  * пропускает уже обработанные UID в рамках процесса;
  * парсит письмо;
  * создаёт viewer page;
  * отправляет Telegram-сообщение;
  * сохраняет связи UID → Telegram message/page.

Поведение, которое надо сохранить:

* Если HTML есть — использовать HTML.
* Если HTML нет, но есть text/plain — экранировать и завернуть в `<pre>`.
* Если нет ни HTML, ни text/plain — письмо пропускается.
* Viewer URL содержит `id` и `token`.
* Токен нельзя логировать.
* `Mark as read` работает через Telegram callback.
* `/mark_read` работает через HTTP, не увеличивая счётчик просмотров.
* `IMAP_MARK_SEEN=true` помечает письмо прочитанным при первом открытии HTML.
* При успешной отметке прочитанным кнопка `Mark as read` скрывается, остаётся только `Open html`.
* Если письмо стало read во внешнем почтовом клиенте, сервис должен скрыть кнопку `Mark as read` при следующем IMAP-полле.
* После рестарта все in-memory ссылки/токены/callback-key недействительны.

## 5. Важные исправления относительно текущего Go-кода

Миграция должна не просто механически переписать код, а закрыть текущие проблемы:

1. **Callback-map cleanup при TTL.**
   Сейчас при удалении страницы по TTL callback-ключи и связи UID/page могут оставаться в памяти. В Rust-версии `on_delete` обязан чистить:

   * `pages`;
   * `page_to_cb_key`;
   * `mark_cb_map`;
   * `uid_to_msg`, если page связана с UID.

2. **Max views без off-by-one.**
   Требуемая семантика: если `VIEWER_PAGE_MAX_VIEWS=3`, первые 3 успешных открытия возвращают `200`, четвёртое возвращает `404`. Текущая Go-логика удаляет страницу только после превышения лимита, фактически позволяя лишний успешный просмотр.

3. **Явная TLS-семантика.**
   В Go-версии `IMAP_TLS=false` фактически означает “TLS без проверки сертификата”, а не plaintext IMAP. В Rust-версии надо убрать эту неоднозначность:

   * `IMAP_TLS=true` — implicit TLS, обычно порт `993`.
   * `IMAP_TLS=false` — legacy compatibility mode, но не должен молча отключать security.
   * Добавить отдельный env `IMAP_ACCEPT_INVALID_CERTS=false`, если реально нужен insecure TLS.
   * Plaintext IMAP должен быть запрещён по умолчанию.

4. **Graceful shutdown.**
   На `SIGTERM`/`SIGINT` сервис должен:

   * остановить HTTP listener;
   * остановить Telegram polling loop;
   * завершить IMAP poll loop;
   * не сохранять никакое состояние на диск.

5. **Конфиг должен fail-fast.**
   Invalid duration/int/bool не должны молча заменяться default-значением. Если env задан, но некорректен — сервис падает на старте с понятной ошибкой.

6. **Очистка expired pages без тысячи таймеров.**
   Вместо `time.AfterFunc` на каждую страницу лучше иметь один cleanup task с интервалом, например 30–60 секунд. Это проще тестировать и лучше масштабируется.

## 6. Целевой Rust-стек

Целевой стек на дату 8 мая 2026:

* `tokio` для async runtime. Актуальная документация показывает `tokio 1.52.2`; Tokio описан как runtime для надёжных async network-приложений и даёт tasks, timers, async I/O и synchronization primitives. ([Docs.rs][2])
* `axum 0.8.9` для HTTP viewer. Axum фокусируется на routing/request handling, extractors, predictable error handling и использует Tower middleware ecosystem. ([Docs.rs][3])
* `tower-http 0.6.10` для HTTP middleware, особенно request/response tracing. ([Docs.rs][4])
* `teloxide 0.17.0` для Telegram Bot API. Это актуальный Rust Telegram bot framework, async и совместимый с Tokio. ([Docs.rs][5])
* `async-imap 0.11.2` для IMAP. Crate поддерживает async IMAP, login, select, fetch/search и работу с RFC 3501 IMAP-серверами. ([Docs.rs][6])
* `mail-parser 0.11.3` для MIME/email parsing. Он поддерживает RFC 5322/MIME, HTML/text body parts, charsets и заявлен как safe Rust без external dependencies. ([Docs.rs][7])
* `ammonia 4.1.2` для HTML sanitization. Ammonia — allowlist-based sanitizer, предназначенный для защиты от XSS/layout breaking/clickjacking; парсит HTML через html5ever по браузерной модели. ([Docs.rs][8])
* `secrecy 0.10.3` для токенов/паролей в конфиге, чтобы случайно не логировать секреты; crate ограничивает exposure секретов и zeroize-ит их при drop. ([Docs.rs][9])
* `tracing 0.1.44` и `tracing-subscriber` для structured logging; `tracing` поддерживает spans/events и может быть drop-in заменой log macros. ([Docs.rs][10])
* `figment` для typed env config. Figment умеет объединять источники конфигурации и извлекать typed values с provenance ошибок. ([Docs.rs][11])

## 7. Предлагаемый `Cargo.toml`

```toml
[package]
name = "mailpuff"
version = "0.1.0"
edition = "2024"
rust-version = "1.94"

[dependencies]
tokio = { version = "1.52", features = ["rt-multi-thread", "macros", "net", "time", "signal", "sync"] }
axum = "0.8.9"
tower-http = { version = "0.6.10", features = ["trace", "timeout", "set-header"] }

teloxide = { version = "0.17.0", default-features = false, features = ["rustls"] }

async-imap = "0.11.2"
tokio-rustls = "0.26.4"
rustls = "0.23.40"
webpki-roots = "1.0"

mail-parser = { version = "0.11.3", features = ["encoding_rs"] }
ammonia = "4.1.2"
html-escape = "0.2.13"

serde = { version = "1.0.228", features = ["derive"] }
figment = { version = "0.10", features = ["env"] }
secrecy = { version = "0.10.3", features = ["serde"] }

uuid = { version = "1.23", features = ["v4", "fast-rng"] }
rand = "0.10"
base64 = "0.22"
url = "2.5"

thiserror = "2.0"
anyhow = "1.0"

tracing = "0.1.44"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }

[dev-dependencies]
tokio-test = "0.4"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json"] }
```

Важно: `Cargo.lock` должен быть закоммичен, потому что это application binary, а не библиотека.

## 8. Целевая структура проекта

```text
src/
  main.rs
  app.rs
  config.rs
  error.rs
  state.rs
  viewer/
    mod.rs
    store.rs
    http.rs
    sanitize.rs
  imap/
    mod.rs
    client.rs
    parser.rs
  telegram/
    mod.rs
    bot.rs
    callbacks.rs
  shutdown.rs
tests/
  viewer_store.rs
  config.rs
  email_parser.rs
  http_viewer.rs
Dockerfile
docker-compose.yml
.env.example
README.md
```

Роль модулей:

* `main.rs` — инициализация tracing, config, app state, graceful shutdown.
* `app.rs` — orchestration: запускает HTTP, Telegram updates, IMAP poll loop, cleanup loop.
* `config.rs` — typed env parsing, validation, defaults.
* `state.rs` — shared in-memory state.
* `viewer/store.rs` — pages, tokens, TTL, max views, first-view callbacks.
* `viewer/http.rs` — `/view`, `/mark_read`, security headers.
* `viewer/sanitize.rs` — Ammonia policy.
* `imap/client.rs` — connect/select/search/fetch/mark_seen.
* `imap/parser.rs` — raw RFC822 → email summary.
* `telegram/bot.rs` — send/edit message.
* `telegram/callbacks.rs` — callback data generation/lookup.
* `shutdown.rs` — cancellation token / signal handling.

## 9. In-memory state model

Все структуры ниже живут только в RAM:

```rust
struct AppState {
    pages: Arc<PageStore>,
    callbacks: Arc<CallbackStore>,
    uid_index: Arc<UidIndex>,
}
```

`PageStore`:

```rust
struct Page {
    id: Uuid,
    token_hash_or_secret: SecretString,
    html: String,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    max_views: Option<u32>,
    views: u32,
    telegram_ref: Option<TelegramMessageRef>,
    imap_uid: Option<u32>,
}
```

`CallbackStore`:

```rust
struct MarkCallbackPayload {
    page_id: Uuid,
    token: SecretString,
}

mark_cb_map: HashMap<String, MarkCallbackPayload>
page_to_cb_key: HashMap<Uuid, String>
```

`UidIndex`:

```rust
processed_uids: HashSet<u32>
uid_to_msg: HashMap<u32, TelegramMessageRef>
```

Concurrency model:

* Use `tokio::sync::RwLock` or `std::sync::RwLock` inside `Arc`.
* For mostly short critical sections, `std::sync::RwLock` is acceptable, but avoid holding lock across `.await`.
* Never return mutable references to internal state across await boundaries.
* For callbacks, clone minimal immutable metadata before spawning async work.

## 10. Functional requirements

### 10.1 Config

Required env:

* `IMAP_HOST`
* `IMAP_USERNAME`
* `IMAP_PASSWORD`
* `TELEGRAM_TOKEN`
* `TELEGRAM_CHAT_ID`
* `VIEWER_URL_BASE`

Optional env with defaults:

* `IMAP_PORT=993`
* `IMAP_TLS=true`
* `IMAP_ACCEPT_INVALID_CERTS=false`
* `IMAP_MAILBOX=INBOX`
* `IMAP_POLL_INTERVAL=60s`
* `IMAP_FORCE_RECONNECT=60s`
* `IMAP_MARK_SEEN=false`
* `HTTP_ADDR=:8080`
* `VIEWER_PAGE_TTL=48h`
* `VIEWER_PAGE_MAX_VIEWS=3`
* `VIEWER_REMOTE_IMAGES=allow`
* `RUST_LOG=info`

Validation:

* `TELEGRAM_CHAT_ID` must parse as `i64`.
* Durations must support Go-like values: `60s`, `5m`, `48h`.
* If env exists but invalid — fail startup.
* `VIEWER_URL_BASE` must be absolute URL and path should end with `/view`.
* `VIEWER_PAGE_MAX_VIEWS <= 0` means unlimited views.
* `VIEWER_PAGE_TTL <= 0` is invalid unless explicitly changed to mean “no TTL”; recommended: invalid.

### 10.2 IMAP polling

Poll loop behavior:

1. Wait immediately or start immediately on boot.
2. Connect to IMAP.
3. Authenticate.
4. Select configured mailbox.
5. Search `UNSEEN`.
6. Compare unseen UID set with `uid_to_msg`:

   * if UID was previously tracked but is no longer unseen, hide `Mark as read`.
7. Fetch unseen UIDs not in `processed_uids`.
8. Parse each email.
9. Create viewer page.
10. Generate Telegram message.
11. Store in-memory mappings.
12. Sleep `IMAP_POLL_INTERVAL`.

Failure behavior:

* Connect/search/fetch errors are logged.
* Poll loop continues after sleep.
* One bad email must not stop processing of other emails.
* Telegram send failure should mark UID processed only if product decision is “do not retry”; recommended behavior: do **not** mark processed on Telegram send failure, so the email is retried next poll.

### 10.3 Email parsing

Email summary fields:

```rust
struct EmailSummary {
    subject: String,
    from_name: Option<String>,
    from_address: Option<String>,
    to_address: Option<String>,
    date: Option<OffsetDateTime>,
    html_body: Option<String>,
}
```

Rules:

* Prefer HTML body.
* If HTML missing and text body exists, escape text and wrap in:

```html
<pre style="white-space:pre-wrap;word-wrap:break-word;"></pre>
```

* If both missing — skip email.
* Subject fallback: `(no subject)`.
* Sender fallback:

  * name: `Unknown sender`
  * address: `unknown@unknown`

### 10.4 Viewer page creation

On page creation:

* Sanitize HTML before saving.
* Reject empty HTML after sanitization.
* Generate UUID v4 page id.
* Generate cryptographically secure URL-safe token.
* Store only in memory.
* Create `expires_at = now + ttl`.

Token requirements:

* At least 128 bits entropy.
* URL-safe base64 without padding is acceptable.
* Token must never appear in logs.
* Token comparison should avoid obvious accidental leaks. Full constant-time compare is nice-to-have, not mandatory for this app, but easy to add.

### 10.5 Viewer `/view`

Endpoint:

```text
GET /view?id=<uuid>&token=<token>
```

Behavior:

* Missing params → `404`.
* Unknown id → `404`.
* Invalid token → `404`.
* Expired page → delete page and return `404`.
* Valid page:

  * increment views;
  * if first view and `IMAP_MARK_SEEN=true`, trigger mark-seen async flow;
  * return sanitized HTML with `Content-Type: text/html; charset=utf-8`;
  * if view count reaches max views, page is deleted after serving this response.

Security headers:

```text
Content-Type: text/html; charset=utf-8
X-Content-Type-Options: nosniff
Referrer-Policy: no-referrer
X-Robots-Tag: noindex, nofollow
Content-Security-Policy: default-src 'none'; img-src http: https: data: cid:; style-src 'unsafe-inline'; font-src data: https:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
```

If `VIEWER_REMOTE_IMAGES=block`, `img-src` should exclude `http:` and `https:`, and sanitizer should remove remote image URLs.

### 10.6 Viewer `/mark_read`

Endpoint:

```text
GET /mark_read?id=<uuid>&token=<token>
```

Behavior:

* Does not increment views.
* Authorizes by id/token.
* Checks page has IMAP UID.
* Calls IMAP `mark_seen`.
* Returns plain text `OK` on success.
* On success should also hide Telegram `Mark as read` button if message ref exists.
* Cleans callback key and UID mapping.

### 10.7 Telegram message

Message text should preserve current intent:

```text
<subject>
<from name>

A new email has arrived from this address: <from address>

🌐 A secret HTML page has been created for it, where you can preview the message by following the link below 👇
```

Requirements:

* Escape HTML entities in subject/from fields.
* Use Telegram HTML parse mode.
* Disable web page preview.
* Inline keyboard:

  * `Open html` URL button.
  * `Mark as read` callback button.
* Callback data format:

```text
mark:<short_callback_key>
```

Callback key:

* Random URL-safe short string.
* Must fit Telegram callback data limit.
* Stored only in RAM.
* Maps to page id + token.

### 10.8 Telegram callback handling

On callback `mark:<key>`:

* If key missing → answer callback `Link expired`.
* If page missing/expired/token invalid → answer callback `Link expired or invalid`.
* If IMAP UID missing → answer callback `IMAP UID missing`.
* If IMAP mark-seen fails → answer callback `Failed to mark as read`.
* On success:

  * answer callback `Marked as read`;
  * edit message reply markup to keep only `Open html`;
  * delete callback key;
  * delete page-to-callback mapping;
  * delete UID-to-message mapping.

### 10.9 Auto-hide mark button

Each IMAP poll produces current unseen UID set.

For every UID in `uid_to_msg`:

* If UID is still unseen — do nothing.
* If UID is absent from unseen set — assume it became read externally.
* Edit Telegram message keyboard to remove `Mark as read`.
* Clean callback maps and `uid_to_msg`.

### 10.10 Cleanup loop

Run periodic cleanup task:

* interval default: 60 seconds or `min(VIEWER_PAGE_TTL / 10, 60s)` with lower bound 10s;
* delete expired pages;
* for every deleted page:

  * clean callback key;
  * clean page-to-callback key;
  * clean uid-to-message if matching;
  * log masked page id and reason.

No disk writes.

## 11. Non-functional requirements

### 11.1 No persistence

Strictly forbidden:

* SQLite
* Postgres
* MySQL
* Redis
* Sled/RocksDB
* file cache
* local JSON state files
* persistent Telegram dialogue storage
* persistent IMAP UID storage

Allowed:

* Reading env vars.
* Docker image layers.
* Docker logs, but without secrets.
* Runtime memory only.

After restart:

* `pages` empty.
* callback maps empty.
* processed UID set empty.
* old Telegram buttons may remain visible, but callback returns `Link expired`.
* old `/view` links return `404`.

### 11.2 Security

Requirements:

* Never log `IMAP_PASSWORD`, `TELEGRAM_TOKEN`, viewer token, callback payload token.
* Mask page id in logs: first 4 + last 4 chars.
* Sanitize all email HTML before storing.
* Escape text/plain fallback.
* Add CSP and no-referrer headers.
* Default TLS certificate validation enabled.
* `IMAP_ACCEPT_INVALID_CERTS=true` allowed only with warning log on startup.
* No plaintext IMAP unless explicitly introduced as a separate unsafe mode; recommended not to support it.

### 11.3 Reliability

* Poll loop survives IMAP errors.
* Telegram loop survives callback processing errors.
* HTTP server returns `404` for invalid links, not `500`.
* Mark-seen failure does not delete page.
* First-view mark-seen failure does not block HTML response.
* Graceful shutdown completes within configurable timeout, e.g. 10 seconds.

### 11.4 Performance

Expected load is small, but design should handle:

* hundreds/thousands of in-memory pages;
* polling every 30–60 seconds;
* multiple concurrent `/view` requests;
* Telegram callback and HTTP mark-read racing safely.

Memory risk:

* HTML email bodies can be large. Add optional `VIEWER_PAGE_MAX_BYTES`, default e.g. `2MiB` or `5MiB`.
* If body exceeds max, skip email or truncate only if explicitly accepted. Recommended: skip and log reason.

## 12. Acceptance criteria

Migration is accepted when:

* Rust app builds with `edition = "2024"` and `rustc >= 1.94`.
* Docker image runs without Go toolchain.
* Same `.env` values from current app work, except documented TLS clarification.
* New unseen email produces one Telegram message.
* Telegram message has `Open html` and `Mark as read`.
* `/view?id=&token=` returns sanitized HTML.
* Invalid token returns `404`.
* Expired page returns `404`.
* With `VIEWER_PAGE_MAX_VIEWS=3`, first 3 opens return `200`, fourth returns `404`.
* `IMAP_MARK_SEEN=true` marks email seen on first valid `/view`.
* Telegram callback `Mark as read` marks email seen and removes the button.
* External read status hides the button on next poll.
* Restart invalidates old links and callback keys.
* No runtime state is written to disk.
* Logs do not contain tokens/passwords.
* Unit/integration tests pass.
* `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` pass.

## 13. Test plan

Unit tests:

* Config parsing:

  * required env missing;
  * invalid duration;
  * invalid chat id;
  * default values.
* URL builder:

  * preserves base URL;
  * sets `id` and `token`;
  * handles existing query params.
* Token generation:

  * URL-safe;
  * sufficient length;
  * uniqueness smoke test.
* Store:

  * create page;
  * authorize valid/invalid token;
  * first view detection;
  * max views exact behavior;
  * TTL expiry;
  * cleanup callback clears maps.
* Sanitizer:

  * removes `<script>`;
  * removes event handlers;
  * keeps basic email table markup;
  * handles text/plain fallback safely.
* Callback store:

  * key lookup;
  * key deletion;
  * expired page cleanup.

Integration tests:

* Start axum app with in-memory store.
* `GET /view` valid/invalid/expired.
* `GET /mark_read` with mock IMAP mark-seen handler.
* Simulate Telegram callback handler with mock bot client if feasible.

Manual E2E:

* Gmail app password mailbox.
* Telegram private chat.
* Send HTML email.
* Open viewer link.
* Press mark-read.
* Mark read externally and confirm button auto-hide.
* Restart container and confirm old link `404`.

## 14. Migration phases

### Phase 1 — Rust skeleton

* Create Rust project.
* Add `Cargo.toml`, modules, CI commands.
* Implement config and logging.
* Add Dockerfile with Rust build.

### Phase 2 — Viewer store and HTTP

* Implement in-memory `PageStore`.
* Implement sanitizer.
* Implement `/view` and `/mark_read`.
* Add tests for TTL/max views/auth.

### Phase 3 — Email parsing and IMAP

* Implement IMAP connect/select/search/fetch.
* Fetch raw RFC822.
* Parse via `mail-parser`.
* Convert to `EmailSummary`.
* Add parser tests with sample MIME messages.

### Phase 4 — Telegram

* Implement send message.
* Implement edit keyboard.
* Implement callback loop.
* Add callback store.

Утверждённые решения для Phase 4:

* Scope Phase 4: без poll loop; только Telegram layer и callback handling.
* Telegram dependency: `teloxide 0.17.0`; использовать Rustls features вместо default native TLS, чтобы Alpine Docker build не зависел от OpenSSL.
* Callback loop включается в runtime сразу, даже если до Phase 5 callback store ещё не наполняется автоматически; это безопасно и даёт корректный `Link expired` для старых кнопок.

### Phase 5 — Orchestration

* Implement poll loop.
* Implement auto-hide logic.
* Implement first-view mark-seen.
* Implement cleanup loop.
* Implement graceful shutdown.

Текущий прогресс:

* 5.1 реализован: provider-neutral RAM `RuntimeState` и общий mark-read service для HTTP и Telegram.
* 5.2 реализован: poll loop MVP через `MailSource` создаёт viewer page/callback, отправляет Telegram message и обновляет RAM indices.
* 5.3 реализован: auto-hide external read сравнивает current unread set с tracked messages, скрывает `Mark as read` и чистит callback/tracked mappings; failed Telegram edit ретраится на следующем poll.
* 5.4 реализован: first-view mark-seen при `IMAP_MARK_SEEN=true` запускает общий mark-read flow после первого успешного `/view` без блокировки HTML response. Cleanup loop и coordinated shutdown остаются pending.

### Phase 6 — Hardening

* Security headers.
* Secret-safe logs.
* Invalid config fail-fast.
* Clippy/test/docs.
* Update README and `.env.example`.

## 15. Open product decisions

Рекомендую закрыть эти решения до кодинга:

1. **Remote images в HTML письмах.**
   Текущий Go sanitizer разрешает `http/https` картинки. Это сохраняет внешний вид писем, но может раскрывать факт открытия письма внешним трекерам. Я бы сделал `VIEWER_REMOTE_IMAGES=allow|block`, default `allow` для совместимости или `block` для privacy-first.

2. **Что делать при Telegram send failure.**
   Текущий Go-код помечает UID processed даже при ошибке отправки, из-за чего письмо не будет ретраиться в рамках процесса. Для Rust лучше не помечать processed до успешной отправки.

3. **`IMAP_FORCE_RECONNECT`.**
   Сейчас переменная загружается, но фактически не используется. В Rust можно либо:

   * оставить reconnect every poll и объявить переменную deprecated;
   * либо сделать persistent IMAP session с forced reconnect.
     Для MVP я бы оставил reconnect every poll: проще, надёжнее, ближе к текущему поведению.

4. **CID images/attachments.**
   Current behavior не резолвит вложения в `cid:` картинки. В Rust PRD тоже не включает хранение attachments, потому что это увеличит RAM footprint. CID можно оставить как разрешённую схему, но фактически многие картинки не откроются.

## 16. Итоговая рекомендация

Делать миграцию как **полную замену бинарника**, а не как параллельный Rust sidecar. Самая важная часть — не переписать синтаксис Go на Rust, а жёстко зафиксировать состояние как RAM-only и убрать неоднозначные/дырявые места: TLS, callback cleanup, max views, error handling и graceful shutdown.

[1]: https://doc.rust-lang.org/edition-guide/rust-2024/index.html "Rust 2024 - The Rust Edition Guide"
[2]: https://docs.rs/tokio "tokio - Rust"
[3]: https://docs.rs/crate/axum/latest "axum 0.8.9 - Docs.rs"
[4]: https://docs.rs/crate/tower-http/latest "tower-http 0.6.10 - Docs.rs"
[5]: https://docs.rs/crate/teloxide/latest "teloxide 0.17.0 - Docs.rs"
[6]: https://docs.rs/async-imap/ "async_imap - Rust"
[7]: https://docs.rs/mail-parser "mail_parser - Rust"
[8]: https://docs.rs/crate/ammonia/latest "ammonia 4.1.2 - Docs.rs"
[9]: https://docs.rs/crate/secrecy/latest "secrecy 0.10.3 - Docs.rs"
[10]: https://docs.rs/crate/tracing/latest "tracing 0.1.44 - Docs.rs"
[11]: https://docs.rs/figment/latest/figment/ "figment - Rust"
