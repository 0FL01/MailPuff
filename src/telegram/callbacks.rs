use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::{ExposeSecret, SecretString};
use teloxide::{dispatching::Dispatcher, dptree, prelude::*, types::CallbackQuery};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    telegram::bot::{TelegramBot, TelegramError, TelegramMessageRef},
    viewer::{
        http::MarkReadHandler,
        store::{PageAccess, PageStore, StoreError},
    },
};

const CALLBACK_PREFIX: &str = "mark:";
const CALLBACK_KEY_BYTES: usize = 12;
const MAX_CALLBACK_DATA_BYTES: usize = 64;
const MAX_CALLBACK_KEY_BYTES: usize = MAX_CALLBACK_DATA_BYTES - CALLBACK_PREFIX.len();
const CALLBACK_KEY_GENERATION_ATTEMPTS: usize = 8;

#[async_trait]
pub trait CallbackTelegramApi: Send + Sync {
    async fn answer_callback(&self, callback_id: &str, text: &str) -> Result<(), TelegramError>;

    async fn edit_open_only_keyboard(
        &self,
        message: TelegramMessageRef,
        viewer_url: Url,
    ) -> Result<(), TelegramError>;
}

#[derive(Debug, Default)]
pub struct CallbackStore {
    inner: RwLock<CallbackStoreInner>,
}

impl CallbackStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        page_id: Uuid,
        token: SecretString,
    ) -> Result<CreatedCallback, CallbackStoreError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| CallbackStoreError::LockPoisoned)?;

        if let Some(existing_key) = inner.page_to_key.remove(&page_id) {
            inner.by_key.remove(&existing_key);
        }

        for _ in 0..CALLBACK_KEY_GENERATION_ATTEMPTS {
            let key = generate_callback_key()?;
            if inner.by_key.contains_key(&key) {
                continue;
            }

            let payload = MarkCallbackPayload { page_id, token };
            inner.by_key.insert(key.clone(), payload);
            inner.page_to_key.insert(page_id, key.clone());

            return Ok(CreatedCallback {
                callback_data: build_mark_callback_data(&key),
                key,
            });
        }

        Err(CallbackStoreError::KeyCollision)
    }

    pub fn lookup(&self, key: &str) -> Result<Option<MarkCallbackPayload>, CallbackStoreError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| CallbackStoreError::LockPoisoned)?
            .by_key
            .get(key)
            .cloned())
    }

    pub fn lookup_page(
        &self,
        page_id: Uuid,
    ) -> Result<Option<MarkCallbackPayload>, CallbackStoreError> {
        let inner = self
            .inner
            .read()
            .map_err(|_| CallbackStoreError::LockPoisoned)?;
        let Some(key) = inner.page_to_key.get(&page_id) else {
            return Ok(None);
        };

        Ok(inner.by_key.get(key).cloned())
    }

    pub fn delete_key(&self, key: &str) -> Result<Option<MarkCallbackPayload>, CallbackStoreError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| CallbackStoreError::LockPoisoned)?;
        let payload = inner.by_key.remove(key);

        if let Some(payload) = &payload
            && inner
                .page_to_key
                .get(&payload.page_id)
                .is_some_and(|stored_key| stored_key == key)
        {
            inner.page_to_key.remove(&payload.page_id);
        }

        Ok(payload)
    }

    pub fn delete_page(
        &self,
        page_id: Uuid,
    ) -> Result<Option<MarkCallbackPayload>, CallbackStoreError> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| CallbackStoreError::LockPoisoned)?;
        let Some(key) = inner.page_to_key.remove(&page_id) else {
            return Ok(None);
        };

        Ok(inner.by_key.remove(&key))
    }

    pub fn len(&self) -> Result<usize, CallbackStoreError> {
        Ok(self
            .inner
            .read()
            .map_err(|_| CallbackStoreError::LockPoisoned)?
            .by_key
            .len())
    }

    pub fn is_empty(&self) -> Result<bool, CallbackStoreError> {
        Ok(self.len()? == 0)
    }
}

#[derive(Debug, Default)]
struct CallbackStoreInner {
    by_key: HashMap<String, MarkCallbackPayload>,
    page_to_key: HashMap<Uuid, String>,
}

#[derive(Clone)]
pub struct MarkCallbackPayload {
    pub page_id: Uuid,
    pub token: SecretString,
}

impl fmt::Debug for MarkCallbackPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MarkCallbackPayload")
            .field("page_id", &self.page_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedCallback {
    pub key: String,
    pub callback_data: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CallbackStoreError {
    #[error("callback key generation failed: {0}")]
    KeyGeneration(String),

    #[error("callback key generation collided too many times")]
    KeyCollision,

    #[error("callback store lock poisoned")]
    LockPoisoned,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkCallbackEvent {
    pub callback_id: String,
    pub data: Option<String>,
    pub message: Option<TelegramMessageRef>,
}

impl MarkCallbackEvent {
    #[must_use]
    pub fn from_query(query: &CallbackQuery) -> Self {
        Self {
            callback_id: query.id.0.clone(),
            data: query.data.clone(),
            message: query
                .regular_message()
                .map(|message| TelegramMessageRef::new(message.chat.id.0, message.id.0)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkCallbackOutcome {
    Ignored,
    InvalidData,
    LinkExpired,
    LinkExpiredOrInvalid,
    MissingMailRef,
    FailedToMarkRead,
    MarkedRead,
}

#[derive(Debug, Error)]
pub enum MarkCallbackError {
    #[error(transparent)]
    CallbackStore(#[from] CallbackStoreError),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Telegram(#[from] TelegramError),
}

pub async fn handle_mark_callback(
    event: MarkCallbackEvent,
    page_store: &PageStore,
    callback_store: &CallbackStore,
    mark_read: &dyn MarkReadHandler,
    telegram: &dyn CallbackTelegramApi,
    viewer_url_base: &Url,
) -> Result<MarkCallbackOutcome, MarkCallbackError> {
    let Some(data) = event.data.as_deref() else {
        return Ok(MarkCallbackOutcome::Ignored);
    };

    let key = match parse_mark_callback_key(data) {
        Ok(key) => key,
        Err(CallbackDataError::NotMarkCallback) => return Ok(MarkCallbackOutcome::Ignored),
        Err(CallbackDataError::InvalidMarkKey) => {
            telegram
                .answer_callback(&event.callback_id, "Invalid data")
                .await?;
            return Ok(MarkCallbackOutcome::InvalidData);
        }
    };

    let Some(payload) = callback_store.lookup(key)? else {
        telegram
            .answer_callback(&event.callback_id, "Link expired")
            .await?;
        return Ok(MarkCallbackOutcome::LinkExpired);
    };

    let page = match page_store.authorize(payload.page_id, payload.token.expose_secret())? {
        PageAccess::Granted(page) => page,
        PageAccess::Denied(_) => {
            callback_store.delete_key(key)?;
            telegram
                .answer_callback(&event.callback_id, "Link expired or invalid")
                .await?;
            return Ok(MarkCallbackOutcome::LinkExpiredOrInvalid);
        }
    };

    if page.mail_ref.is_none() {
        telegram
            .answer_callback(&event.callback_id, "IMAP UID missing")
            .await?;
        return Ok(MarkCallbackOutcome::MissingMailRef);
    }

    let Some(message) = event.message else {
        telegram
            .answer_callback(&event.callback_id, "Link expired or invalid")
            .await?;
        return Ok(MarkCallbackOutcome::LinkExpiredOrInvalid);
    };

    let mark_read_result = match mark_read.mark_read(page).await {
        Ok(result) => result,
        Err(_) => {
            telegram
                .answer_callback(&event.callback_id, "Failed to mark as read")
                .await?;
            return Ok(MarkCallbackOutcome::FailedToMarkRead);
        }
    };

    if !mark_read_result.keyboard_hidden {
        telegram
            .edit_open_only_keyboard(
                message,
                build_viewer_url(
                    viewer_url_base,
                    payload.page_id,
                    payload.token.expose_secret(),
                ),
            )
            .await?;
    }

    if !mark_read_result.callback_deleted {
        callback_store.delete_key(key)?;
    }

    telegram
        .answer_callback(&event.callback_id, "Marked as read")
        .await?;

    Ok(MarkCallbackOutcome::MarkedRead)
}

pub async fn run_callback_loop(
    bot: Arc<TelegramBot>,
    page_store: Arc<PageStore>,
    callback_store: Arc<CallbackStore>,
    mark_read: Arc<dyn MarkReadHandler>,
    viewer_url_base: Url,
) {
    let raw_bot = bot.raw().clone();
    let handler = Update::filter_callback_query().endpoint(handle_teloxide_callback);

    Dispatcher::builder(raw_bot, handler)
        .dependencies(dptree::deps![
            bot,
            page_store,
            callback_store,
            mark_read,
            viewer_url_base
        ])
        .build()
        .dispatch()
        .await;
}

async fn handle_teloxide_callback(
    query: CallbackQuery,
    bot: Arc<TelegramBot>,
    page_store: Arc<PageStore>,
    callback_store: Arc<CallbackStore>,
    mark_read: Arc<dyn MarkReadHandler>,
    viewer_url_base: Url,
) -> Result<(), std::convert::Infallible> {
    let event = MarkCallbackEvent::from_query(&query);
    match handle_mark_callback(
        event,
        page_store.as_ref(),
        callback_store.as_ref(),
        mark_read.as_ref(),
        bot.as_ref(),
        &viewer_url_base,
    )
    .await
    {
        Ok(MarkCallbackOutcome::Ignored) => {}
        Ok(outcome) => tracing::info!(?outcome, "telegram mark-read callback handled"),
        Err(error) => tracing::error!(%error, "telegram mark-read callback failed"),
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackDataError {
    NotMarkCallback,
    InvalidMarkKey,
}

#[must_use]
pub fn build_mark_callback_data(key: &str) -> String {
    format!("{CALLBACK_PREFIX}{key}")
}

pub fn parse_mark_callback_key(data: &str) -> Result<&str, CallbackDataError> {
    let Some(key) = data.strip_prefix(CALLBACK_PREFIX) else {
        return Err(CallbackDataError::NotMarkCallback);
    };

    if !is_valid_callback_key(key) {
        return Err(CallbackDataError::InvalidMarkKey);
    }

    Ok(key)
}

fn is_valid_callback_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_CALLBACK_KEY_BYTES
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub fn build_viewer_url(base: &Url, id: Uuid, token: &str) -> Url {
    let mut url = base.clone();
    let existing_pairs = url
        .query_pairs()
        .filter(|(key, _)| key != "id" && key != "token")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();

    url.set_query(None);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in existing_pairs {
            pairs.append_pair(&key, &value);
        }
        pairs.append_pair("id", &id.to_string());
        pairs.append_pair("token", token);
    }

    url
}

fn generate_callback_key() -> Result<String, CallbackStoreError> {
    let mut bytes = [0_u8; CALLBACK_KEY_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|error| CallbackStoreError::KeyGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::{sync::Mutex, time::Duration};

    use secrecy::ExposeSecret;

    use crate::{
        config::ViewerRemoteImages,
        mail_source::{MailSourceKind, MessageRef},
        viewer::http::{MarkReadError, MarkReadResult},
        viewer::store::{AuthorizedPage, CreatePageOptions, PageStoreConfig},
    };

    use super::*;

    #[derive(Debug, Default)]
    struct FakeTelegram {
        answers: Mutex<Vec<String>>,
        edits: Mutex<Vec<(TelegramMessageRef, Url)>>,
    }

    #[async_trait]
    impl CallbackTelegramApi for FakeTelegram {
        async fn answer_callback(
            &self,
            _callback_id: &str,
            text: &str,
        ) -> Result<(), TelegramError> {
            self.answers
                .lock()
                .expect("answers lock")
                .push(text.to_owned());
            Ok(())
        }

        async fn edit_open_only_keyboard(
            &self,
            message: TelegramMessageRef,
            viewer_url: Url,
        ) -> Result<(), TelegramError> {
            self.edits
                .lock()
                .expect("edits lock")
                .push((message, viewer_url));
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct OkMarkRead {
        calls: Mutex<usize>,
    }

    #[async_trait]
    impl MarkReadHandler for OkMarkRead {
        async fn mark_read(&self, _page: AuthorizedPage) -> Result<MarkReadResult, MarkReadError> {
            *self.calls.lock().expect("calls lock") += 1;
            Ok(MarkReadResult::default())
        }
    }

    #[derive(Debug, Default)]
    struct FailingMarkRead;

    #[async_trait]
    impl MarkReadHandler for FailingMarkRead {
        async fn mark_read(&self, _page: AuthorizedPage) -> Result<MarkReadResult, MarkReadError> {
            Err(MarkReadError::Backend("boom".to_owned()))
        }
    }

    #[derive(Debug)]
    struct SideEffectMarkRead {
        callback_store: Arc<CallbackStore>,
    }

    #[async_trait]
    impl MarkReadHandler for SideEffectMarkRead {
        async fn mark_read(&self, page: AuthorizedPage) -> Result<MarkReadResult, MarkReadError> {
            let callback_deleted = self
                .callback_store
                .delete_page(page.id)
                .map_err(|error| MarkReadError::Backend(error.to_string()))?
                .is_some();

            Ok(MarkReadResult {
                keyboard_hidden: true,
                callback_deleted,
            })
        }
    }

    fn page_store(ttl: Duration) -> PageStore {
        PageStore::new(PageStoreConfig {
            page_ttl: ttl,
            page_max_views: Some(3),
            remote_images: ViewerRemoteImages::Allow,
        })
    }

    fn mail_ref() -> MessageRef {
        MessageRef::new(
            MailSourceKind::Imap,
            "imap.example.com",
            Some("INBOX".to_owned()),
            "42",
        )
    }

    fn event(callback_data: String) -> MarkCallbackEvent {
        MarkCallbackEvent {
            callback_id: "cb-id".to_owned(),
            data: Some(callback_data),
            message: Some(TelegramMessageRef::new(100, 200)),
        }
    }

    #[test]
    fn callback_store_creates_url_safe_key_and_data() -> Result<(), Box<dyn std::error::Error>> {
        let store = CallbackStore::new();
        let page_id = Uuid::new_v4();
        let callback = store.create(page_id, SecretString::from("secret-token".to_owned()))?;

        assert!(callback.callback_data.starts_with(CALLBACK_PREFIX));
        assert!(is_valid_callback_key(&callback.key));
        assert!(callback.callback_data.len() <= MAX_CALLBACK_DATA_BYTES);

        let payload = store.lookup(&callback.key)?.expect("payload exists");
        assert_eq!(payload.page_id, page_id);
        assert_eq!(payload.token.expose_secret(), "secret-token");
        assert_eq!(
            store
                .lookup_page(page_id)?
                .expect("page payload")
                .token
                .expose_secret(),
            "secret-token"
        );

        Ok(())
    }

    #[test]
    fn callback_store_deletes_by_key_and_page() -> Result<(), Box<dyn std::error::Error>> {
        let store = CallbackStore::new();
        let first_page_id = Uuid::new_v4();
        let second_page_id = Uuid::new_v4();
        let first = store.create(first_page_id, SecretString::from("one".to_owned()))?;
        let second = store.create(second_page_id, SecretString::from("two".to_owned()))?;

        assert!(store.delete_key(&first.key)?.is_some());
        assert!(store.lookup(&first.key)?.is_none());
        assert!(store.delete_page(second_page_id)?.is_some());
        assert!(store.lookup(&second.key)?.is_none());
        assert!(store.is_empty()?);

        Ok(())
    }

    #[test]
    fn parses_only_valid_mark_callback_data() {
        assert_eq!(
            parse_mark_callback_key("mark:abc_DEF-123"),
            Ok("abc_DEF-123")
        );
        assert_eq!(
            parse_mark_callback_key("noop:abc"),
            Err(CallbackDataError::NotMarkCallback)
        );
        assert_eq!(
            parse_mark_callback_key("mark:"),
            Err(CallbackDataError::InvalidMarkKey)
        );
        assert_eq!(
            parse_mark_callback_key("mark:bad/key"),
            Err(CallbackDataError::InvalidMarkKey)
        );
    }

    #[test]
    fn viewer_url_preserves_existing_query_and_replaces_id_token() -> Result<(), url::ParseError> {
        let base = Url::parse("https://mail.example.com/view?lang=en&id=old&token=old")?;
        let page_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").expect("static uuid");

        let url = build_viewer_url(&base, page_id, "secret token");

        assert_eq!(
            url.as_str(),
            "https://mail.example.com/view?lang=en&id=00000000-0000-4000-8000-000000000001&token=secret+token"
        );

        Ok(())
    }

    #[tokio::test]
    async fn mark_callback_success_marks_read_edits_keyboard_and_deletes_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let page_store = page_store(Duration::from_secs(60));
        let callback_store = CallbackStore::new();
        let page = page_store.create_page_with_options(
            "<p>Hello</p>",
            CreatePageOptions {
                mail_ref: Some(mail_ref()),
            },
        )?;
        let callback = callback_store.create(page.id, page.token.clone())?;
        let telegram = FakeTelegram::default();
        let mark_read = OkMarkRead::default();
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        let outcome = handle_mark_callback(
            event(callback.callback_data.clone()),
            &page_store,
            &callback_store,
            &mark_read,
            &telegram,
            &viewer_url_base,
        )
        .await?;

        assert_eq!(outcome, MarkCallbackOutcome::MarkedRead);
        assert!(callback_store.lookup(&callback.key)?.is_none());
        assert_eq!(*mark_read.calls.lock().expect("calls lock"), 1);
        assert_eq!(
            telegram.answers.lock().expect("answers lock").as_slice(),
            ["Marked as read"]
        );
        let edits = telegram.edits.lock().expect("edits lock");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].0, TelegramMessageRef::new(100, 200));
        assert_eq!(
            edits[0].1.as_str(),
            format!(
                "https://mail.example.com/view?id={}&token={}",
                page.id,
                page.token.expose_secret()
            )
        );

        Ok(())
    }

    #[tokio::test]
    async fn mark_callback_skips_duplicate_side_effects() -> Result<(), Box<dyn std::error::Error>>
    {
        let page_store = page_store(Duration::from_secs(60));
        let callback_store = Arc::new(CallbackStore::new());
        let page = page_store.create_page_with_options(
            "<p>Hello</p>",
            CreatePageOptions {
                mail_ref: Some(mail_ref()),
            },
        )?;
        let callback = callback_store.create(page.id, page.token.clone())?;
        let telegram = FakeTelegram::default();
        let mark_read = SideEffectMarkRead {
            callback_store: Arc::clone(&callback_store),
        };
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        let outcome = handle_mark_callback(
            event(callback.callback_data.clone()),
            &page_store,
            callback_store.as_ref(),
            &mark_read,
            &telegram,
            &viewer_url_base,
        )
        .await?;

        assert_eq!(outcome, MarkCallbackOutcome::MarkedRead);
        assert!(callback_store.lookup(&callback.key)?.is_none());
        assert!(telegram.edits.lock().expect("edits lock").is_empty());
        assert_eq!(
            telegram.answers.lock().expect("answers lock").as_slice(),
            ["Marked as read"]
        );

        Ok(())
    }

    #[tokio::test]
    async fn missing_key_answers_link_expired() -> Result<(), Box<dyn std::error::Error>> {
        let page_store = page_store(Duration::from_secs(60));
        let callback_store = CallbackStore::new();
        let telegram = FakeTelegram::default();
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        let outcome = handle_mark_callback(
            event("mark:missing".to_owned()),
            &page_store,
            &callback_store,
            &OkMarkRead::default(),
            &telegram,
            &viewer_url_base,
        )
        .await?;

        assert_eq!(outcome, MarkCallbackOutcome::LinkExpired);
        assert_eq!(
            telegram.answers.lock().expect("answers lock").as_slice(),
            ["Link expired"]
        );

        Ok(())
    }

    #[tokio::test]
    async fn missing_mail_ref_answers_imap_uid_missing() -> Result<(), Box<dyn std::error::Error>> {
        let page_store = page_store(Duration::from_secs(60));
        let callback_store = CallbackStore::new();
        let page = page_store.create_page("<p>Hello</p>")?;
        let callback = callback_store.create(page.id, page.token.clone())?;
        let telegram = FakeTelegram::default();
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        let outcome = handle_mark_callback(
            event(callback.callback_data),
            &page_store,
            &callback_store,
            &OkMarkRead::default(),
            &telegram,
            &viewer_url_base,
        )
        .await?;

        assert_eq!(outcome, MarkCallbackOutcome::MissingMailRef);
        assert_eq!(
            telegram.answers.lock().expect("answers lock").as_slice(),
            ["IMAP UID missing"]
        );

        Ok(())
    }

    #[tokio::test]
    async fn mark_read_failure_keeps_callback_key() -> Result<(), Box<dyn std::error::Error>> {
        let page_store = page_store(Duration::from_secs(60));
        let callback_store = CallbackStore::new();
        let page = page_store.create_page_with_options(
            "<p>Hello</p>",
            CreatePageOptions {
                mail_ref: Some(mail_ref()),
            },
        )?;
        let callback = callback_store.create(page.id, page.token.clone())?;
        let telegram = FakeTelegram::default();
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        let outcome = handle_mark_callback(
            event(callback.callback_data.clone()),
            &page_store,
            &callback_store,
            &FailingMarkRead,
            &telegram,
            &viewer_url_base,
        )
        .await?;

        assert_eq!(outcome, MarkCallbackOutcome::FailedToMarkRead);
        assert!(callback_store.lookup(&callback.key)?.is_some());
        assert_eq!(
            telegram.answers.lock().expect("answers lock").as_slice(),
            ["Failed to mark as read"]
        );

        Ok(())
    }
}
