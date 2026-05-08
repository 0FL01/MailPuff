use std::{collections::HashSet, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use secrecy::ExposeSecret;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    email::parser::parse_email_summary,
    mail_source::{MailSource, MessageRef},
    state::{RuntimeState, RuntimeStateError, TrackedMessage},
    telegram::{
        CallbackStore, SendEmailMessage, TelegramBot, TelegramError, TelegramMessageRef,
        callbacks::{CallbackStoreError, CallbackTelegramApi, build_viewer_url},
    },
    viewer::{
        http::{
            MarkReadError, MarkReadHandler, MarkReadResult, PageDeletionError, PageDeletionHandler,
            PageDeletionResult, masked_uuid,
        },
        store::{AuthorizedPage, CreatePageOptions, DeletedPage, PageStore, StoreError},
    },
};

#[async_trait]
pub trait EmailTelegramApi: CallbackTelegramApi + Send + Sync {
    async fn send_email_message(
        &self,
        request: SendEmailMessage<'_>,
    ) -> Result<TelegramMessageRef, SendTelegramError>;
}

#[async_trait]
impl EmailTelegramApi for TelegramBot {
    async fn send_email_message(
        &self,
        request: SendEmailMessage<'_>,
    ) -> Result<TelegramMessageRef, SendTelegramError> {
        Ok(TelegramBot::send_email_message(self, request).await?)
    }
}

pub struct PollService {
    mail_source: Arc<dyn MailSource>,
    page_store: Arc<PageStore>,
    state: Arc<RuntimeState>,
    callback_store: Arc<CallbackStore>,
    telegram: Arc<dyn EmailTelegramApi>,
    viewer_url_base: Url,
}

impl PollService {
    #[must_use]
    pub fn new(
        mail_source: Arc<dyn MailSource>,
        page_store: Arc<PageStore>,
        state: Arc<RuntimeState>,
        callback_store: Arc<CallbackStore>,
        telegram: Arc<dyn EmailTelegramApi>,
        viewer_url_base: Url,
    ) -> Self {
        Self {
            mail_source,
            page_store,
            state,
            callback_store,
            telegram,
            viewer_url_base,
        }
    }

    pub async fn poll_once(&self) -> Result<PollCycleStats, PollError> {
        let unread = self.mail_source.list_unread().await?;
        let unread_set = unread.iter().cloned().collect::<HashSet<_>>();
        let mut stats = PollCycleStats {
            unread: unread.len(),
            ..PollCycleStats::default()
        };

        self.auto_hide_externally_read(&unread_set, &mut stats)
            .await?;

        for message in unread {
            self.process_unread_message(message, &mut stats).await?;
        }

        Ok(stats)
    }

    async fn process_unread_message(
        &self,
        message: MessageRef,
        stats: &mut PollCycleStats,
    ) -> Result<(), PollError> {
        if self.state.is_processed(&message)? {
            stats.skipped_processed += 1;
            return Ok(());
        }

        let raw = match self.mail_source.fetch(&message).await {
            Ok(raw) => raw,
            Err(error) => {
                stats.fetch_errors += 1;
                tracing::error!(
                    source = %message.source,
                    stable_id = %message.stable_id,
                    %error,
                    "mail fetch failed"
                );
                return Ok(());
            }
        };
        let mail_ref = raw.source_ref.clone();

        let email = match parse_email_summary(&raw.bytes) {
            Ok(email) => email,
            Err(error) => {
                stats.parse_errors += 1;
                let _ = self.state.mark_processed(mail_ref.clone())?;
                tracing::error!(
                    source = %mail_ref.source,
                    stable_id = %mail_ref.stable_id,
                    %error,
                    "email parse failed; message marked processed"
                );
                return Ok(());
            }
        };

        let Some(html_body) = email
            .html_body
            .as_deref()
            .filter(|body| !body.trim().is_empty())
        else {
            stats.skipped_no_body += 1;
            let _ = self.state.mark_processed(mail_ref.clone())?;
            tracing::info!(
                source = %mail_ref.source,
                stable_id = %mail_ref.stable_id,
                "email skipped without body; message marked processed"
            );
            return Ok(());
        };

        let page = match self.page_store.create_page_with_options(
            html_body,
            CreatePageOptions {
                mail_ref: Some(mail_ref.clone()),
            },
        ) {
            Ok(page) => page,
            Err(error) => {
                stats.store_errors += 1;
                tracing::error!(
                    source = %mail_ref.source,
                    stable_id = %mail_ref.stable_id,
                    %error,
                    "viewer page creation failed"
                );
                return Ok(());
            }
        };

        let callback = match self.callback_store.create(page.id, page.token.clone()) {
            Ok(callback) => callback,
            Err(error) => {
                stats.callback_errors += 1;
                self.delete_created_page(page.id);
                tracing::error!(
                    source = %mail_ref.source,
                    stable_id = %mail_ref.stable_id,
                    %error,
                    "telegram callback creation failed"
                );
                return Ok(());
            }
        };

        let viewer_url =
            build_viewer_url(&self.viewer_url_base, page.id, page.token.expose_secret());
        let telegram_ref = match self
            .telegram
            .send_email_message(SendEmailMessage {
                email: &email,
                viewer_url,
                mark_callback_data: callback.callback_data,
            })
            .await
        {
            Ok(telegram_ref) => telegram_ref,
            Err(error) => {
                stats.telegram_errors += 1;
                self.delete_callback_and_page(page.id);
                tracing::error!(
                    source = %mail_ref.source,
                    stable_id = %mail_ref.stable_id,
                    %error,
                    "telegram send failed"
                );
                return Ok(());
            }
        };

        let _ = self.state.mark_processed(mail_ref.clone())?;
        self.state
            .track_message(mail_ref.clone(), page.id, telegram_ref)?;
        stats.processed += 1;

        tracing::info!(
            source = %mail_ref.source,
            stable_id = %mail_ref.stable_id,
            telegram_chat_id = telegram_ref.chat_id,
            telegram_message_id = telegram_ref.message_id,
            "mail message sent to telegram"
        );

        Ok(())
    }

    async fn auto_hide_externally_read(
        &self,
        unread: &HashSet<MessageRef>,
        stats: &mut PollCycleStats,
    ) -> Result<(), PollError> {
        for tracked in self.state.tracked_messages()? {
            if unread.contains(&tracked.mail_ref) {
                continue;
            }

            self.auto_hide_tracked_message(tracked, stats).await?;
        }

        Ok(())
    }

    async fn auto_hide_tracked_message(
        &self,
        tracked: TrackedMessage,
        stats: &mut PollCycleStats,
    ) -> Result<(), PollError> {
        let Some(callback_payload) = self.callback_store.lookup_page(tracked.page_id)? else {
            stats.stale_tracked += 1;
            self.remove_tracked_message(&tracked.mail_ref, tracked.page_id)?;
            tracing::warn!(
                source = %tracked.mail_ref.source,
                stable_id = %tracked.mail_ref.stable_id,
                "tracked message has no callback payload; stale tracking removed"
            );
            return Ok(());
        };

        let viewer_url = build_viewer_url(
            &self.viewer_url_base,
            tracked.page_id,
            callback_payload.token.expose_secret(),
        );

        if let Err(error) = self
            .telegram
            .edit_open_only_keyboard(tracked.telegram_ref, viewer_url)
            .await
        {
            stats.auto_hide_errors += 1;
            tracing::error!(
                source = %tracked.mail_ref.source,
                stable_id = %tracked.mail_ref.stable_id,
                %error,
                "failed to auto-hide telegram mark-read button"
            );
            return Ok(());
        }

        let _ = self.callback_store.delete_page(tracked.page_id)?;
        self.remove_tracked_message(&tracked.mail_ref, tracked.page_id)?;
        stats.auto_hidden += 1;

        tracing::info!(
            source = %tracked.mail_ref.source,
            stable_id = %tracked.mail_ref.stable_id,
            telegram_chat_id = tracked.telegram_ref.chat_id,
            telegram_message_id = tracked.telegram_ref.message_id,
            "external read detected; telegram mark-read button hidden"
        );

        Ok(())
    }

    fn remove_tracked_message(
        &self,
        mail_ref: &MessageRef,
        page_id: uuid::Uuid,
    ) -> Result<(), PollError> {
        let removed = self.state.remove_tracked(mail_ref)?;
        if removed.is_none() {
            let _ = self.state.remove_tracked_by_page(page_id)?;
        }

        Ok(())
    }

    fn delete_callback_and_page(&self, page_id: uuid::Uuid) {
        if let Err(error) = self.callback_store.delete_page(page_id) {
            tracing::error!(%error, "failed to cleanup callback after poll error");
        }
        self.delete_created_page(page_id);
    }

    fn delete_created_page(&self, page_id: uuid::Uuid) {
        if let Err(error) = self.page_store.delete(page_id) {
            tracing::error!(%error, "failed to cleanup viewer page after poll error");
        }
    }
}

pub async fn run_poll_loop(
    service: PollService,
    poll_interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        if shutdown.is_cancelled() {
            break;
        }

        match service.poll_once().await {
            Ok(stats) => tracing::info!(?stats, "mail poll cycle completed"),
            Err(error) => tracing::error!(%error, "mail poll cycle failed"),
        }

        if wait_for_interval_or_shutdown(poll_interval, &shutdown).await {
            break;
        }
    }

    tracing::info!("mail poll loop stopped");
}

pub struct CleanupService {
    page_store: Arc<PageStore>,
    state: Arc<RuntimeState>,
    callback_store: Arc<CallbackStore>,
}

impl CleanupService {
    #[must_use]
    pub fn new(
        page_store: Arc<PageStore>,
        state: Arc<RuntimeState>,
        callback_store: Arc<CallbackStore>,
    ) -> Self {
        Self {
            page_store,
            state,
            callback_store,
        }
    }

    pub fn cleanup_expired_once(&self) -> Result<CleanupStats, CleanupError> {
        let deleted = self.page_store.cleanup_expired()?;
        let mut stats = CleanupStats {
            deleted_pages: deleted.len(),
            ..CleanupStats::default()
        };

        for page in deleted {
            let result = self.handle_deleted_page(page.clone())?;
            stats.callbacks_deleted += usize::from(result.callback_deleted);
            stats.tracked_removed += usize::from(result.tracked_removed);
            tracing::info!(
                page_id = %masked_uuid(page.id),
                reason = ?page.reason,
                callback_deleted = result.callback_deleted,
                tracked_removed = result.tracked_removed,
                "viewer page cleanup completed"
            );
        }

        Ok(stats)
    }

    fn handle_deleted_page(&self, page: DeletedPage) -> Result<PageDeletionResult, CleanupError> {
        let callback_deleted = self.callback_store.delete_page(page.id)?.is_some();
        let mut tracked_removed = self.state.remove_tracked_by_page(page.id)?.is_some();

        if !tracked_removed && let Some(mail_ref) = &page.mail_ref {
            tracked_removed = self.state.remove_tracked(mail_ref)?.is_some();
        }

        Ok(PageDeletionResult {
            callback_deleted,
            tracked_removed,
        })
    }
}

impl PageDeletionHandler for CleanupService {
    fn page_deleted(&self, page: DeletedPage) -> Result<PageDeletionResult, PageDeletionError> {
        self.handle_deleted_page(page)
            .map_err(|error| PageDeletionError::Backend(error.to_string()))
    }
}

pub async fn run_cleanup_loop(
    service: Arc<CleanupService>,
    cleanup_interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        if shutdown.is_cancelled() {
            break;
        }

        match service.cleanup_expired_once() {
            Ok(stats) => tracing::info!(?stats, "viewer cleanup cycle completed"),
            Err(error) => tracing::error!(%error, "viewer cleanup cycle failed"),
        }

        if wait_for_interval_or_shutdown(cleanup_interval, &shutdown).await {
            break;
        }
    }

    tracing::info!("viewer cleanup loop stopped");
}

async fn wait_for_interval_or_shutdown(interval: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        () = shutdown.cancelled() => true,
        () = tokio::time::sleep(interval) => false,
    }
}

#[must_use]
pub fn cleanup_interval(page_ttl: Duration) -> Duration {
    const MIN_INTERVAL: Duration = Duration::from_secs(10);
    const MAX_INTERVAL: Duration = Duration::from_secs(60);

    let tenth = page_ttl / 10;
    std::cmp::min(std::cmp::max(tenth, MIN_INTERVAL), MAX_INTERVAL)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CleanupStats {
    pub deleted_pages: usize,
    pub callbacks_deleted: usize,
    pub tracked_removed: usize,
}

#[derive(Debug, Error)]
pub enum CleanupError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    RuntimeState(#[from] RuntimeStateError),

    #[error(transparent)]
    CallbackStore(#[from] CallbackStoreError),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PollCycleStats {
    pub unread: usize,
    pub processed: usize,
    pub auto_hidden: usize,
    pub skipped_processed: usize,
    pub skipped_no_body: usize,
    pub fetch_errors: usize,
    pub parse_errors: usize,
    pub store_errors: usize,
    pub callback_errors: usize,
    pub telegram_errors: usize,
    pub auto_hide_errors: usize,
    pub stale_tracked: usize,
}

#[derive(Debug, Error)]
pub enum PollError {
    #[error(transparent)]
    App(#[from] crate::error::Error),

    #[error(transparent)]
    RuntimeState(#[from] RuntimeStateError),

    #[error(transparent)]
    CallbackStore(#[from] CallbackStoreError),
}

#[derive(Debug, Error)]
pub enum SendTelegramError {
    #[error(transparent)]
    Telegram(#[from] TelegramError),

    #[error("telegram send failed: {0}")]
    Backend(String),
}

pub struct MarkReadService {
    mail_source: Arc<dyn MailSource>,
    state: Arc<RuntimeState>,
    callback_store: Arc<CallbackStore>,
    telegram: Arc<dyn CallbackTelegramApi>,
    viewer_url_base: Url,
}

impl MarkReadService {
    #[must_use]
    pub fn new(
        mail_source: Arc<dyn MailSource>,
        state: Arc<RuntimeState>,
        callback_store: Arc<CallbackStore>,
        telegram: Arc<dyn CallbackTelegramApi>,
        viewer_url_base: Url,
    ) -> Self {
        Self {
            mail_source,
            state,
            callback_store,
            telegram,
            viewer_url_base,
        }
    }
}

#[async_trait]
impl MarkReadHandler for MarkReadService {
    async fn mark_read(&self, page: AuthorizedPage) -> Result<MarkReadResult, MarkReadError> {
        let Some(mail_ref) = page.mail_ref.clone() else {
            return Err(MarkReadError::Backend(
                "page has no mail source ref".to_owned(),
            ));
        };

        self.mail_source
            .mark_read(&mail_ref)
            .await
            .map_err(backend_error)?;

        let tracked = self
            .state
            .get_tracked(&mail_ref)
            .map_err(backend_error)?
            .or(self
                .state
                .get_tracked_by_page(page.id)
                .map_err(backend_error)?);
        let callback_payload = self
            .callback_store
            .lookup_page(page.id)
            .map_err(backend_error)?;
        let mut result = MarkReadResult::default();

        if let (Some(tracked), Some(callback_payload)) = (&tracked, &callback_payload) {
            let viewer_url = build_viewer_url(
                &self.viewer_url_base,
                page.id,
                callback_payload.token.expose_secret(),
            );
            self.telegram
                .edit_open_only_keyboard(tracked.telegram_ref, viewer_url)
                .await
                .map_err(backend_error)?;
            result.keyboard_hidden = true;
        }

        result.callback_deleted = self
            .callback_store
            .delete_page(page.id)
            .map_err(backend_error)?
            .is_some();

        let removed = self
            .state
            .remove_tracked(&mail_ref)
            .map_err(backend_error)?;
        if removed.is_none() {
            self.state
                .remove_tracked_by_page(page.id)
                .map_err(backend_error)?;
        }

        Ok(result)
    }
}

fn backend_error(error: impl fmt::Display) -> MarkReadError {
    MarkReadError::Backend(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        sync::Mutex,
        time::{Duration, Instant},
    };

    use crate::{
        config::ViewerRemoteImages,
        error::{Error, Result},
        mail_source::{MailSourceCapabilities, MailSourceKind, MessageRef, RawEmail},
        telegram::{TelegramError, TelegramMessageRef},
        viewer::store::{DeletionReason, PageStoreConfig},
    };

    use secrecy::SecretString;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeMailSource {
        unread: Mutex<Vec<MessageRef>>,
        raw: Mutex<HashMap<MessageRef, Vec<u8>>>,
        fetch_failures: Mutex<HashSet<MessageRef>>,
        fetched: Mutex<Vec<MessageRef>>,
        marked: Mutex<Vec<MessageRef>>,
        fail_mark_read: bool,
    }

    impl FakeMailSource {
        fn set_unread(&self, messages: Vec<MessageRef>) {
            *self.unread.lock().expect("unread lock") = messages;
        }

        fn set_raw(&self, message: MessageRef, raw: impl Into<Vec<u8>>) {
            self.raw
                .lock()
                .expect("raw lock")
                .insert(message, raw.into());
        }
    }

    #[async_trait]
    impl MailSource for FakeMailSource {
        async fn list_unread(&self) -> Result<Vec<MessageRef>> {
            Ok(self.unread.lock().expect("unread lock").clone())
        }

        async fn fetch(&self, message: &MessageRef) -> Result<RawEmail> {
            self.fetched
                .lock()
                .expect("fetched lock")
                .push(message.clone());
            if self
                .fetch_failures
                .lock()
                .expect("fetch failures lock")
                .contains(message)
            {
                return Err(Error::NotImplemented("fake fetch failure"));
            }

            Ok(RawEmail::new(
                self.raw
                    .lock()
                    .expect("raw lock")
                    .get(message)
                    .cloned()
                    .unwrap_or_default(),
                message.clone(),
            ))
        }

        async fn mark_read(&self, message: &MessageRef) -> Result<()> {
            if self.fail_mark_read {
                return Err(Error::NotImplemented("fake mark-read failure"));
            }

            self.marked
                .lock()
                .expect("marked lock")
                .push(message.clone());
            Ok(())
        }

        fn capabilities(&self) -> MailSourceCapabilities {
            MailSourceCapabilities::imap()
        }
    }

    #[derive(Debug, Default)]
    struct FakeTelegram {
        edits: Mutex<Vec<(TelegramMessageRef, Url)>>,
        sent: Mutex<Vec<SentEmail>>,
        fail_send: bool,
        fail_edit: bool,
    }

    #[derive(Debug, Clone)]
    struct SentEmail {
        subject: String,
        viewer_url: Url,
        mark_callback_data: String,
    }

    #[async_trait]
    impl CallbackTelegramApi for FakeTelegram {
        async fn answer_callback(
            &self,
            _callback_id: &str,
            _text: &str,
        ) -> std::result::Result<(), TelegramError> {
            Ok(())
        }

        async fn edit_open_only_keyboard(
            &self,
            message: TelegramMessageRef,
            viewer_url: Url,
        ) -> std::result::Result<(), TelegramError> {
            if self.fail_edit {
                return Err(TelegramError::Backend("boom".to_owned()));
            }

            self.edits
                .lock()
                .expect("edits lock")
                .push((message, viewer_url));
            Ok(())
        }
    }

    #[async_trait]
    impl EmailTelegramApi for FakeTelegram {
        async fn send_email_message(
            &self,
            request: SendEmailMessage<'_>,
        ) -> std::result::Result<TelegramMessageRef, SendTelegramError> {
            if self.fail_send {
                return Err(SendTelegramError::Backend("boom".to_owned()));
            }

            let mut sent = self.sent.lock().expect("sent lock");
            sent.push(SentEmail {
                subject: request.email.subject.clone(),
                viewer_url: request.viewer_url,
                mark_callback_data: request.mark_callback_data,
            });

            Ok(TelegramMessageRef::new(100, 200 + sent.len() as i32))
        }
    }

    fn mail_ref() -> MessageRef {
        MessageRef::new(
            MailSourceKind::Imap,
            "imap.example.com",
            Some("INBOX".to_owned()),
            "42",
        )
    }

    fn mail_ref_with_id(stable_id: &str) -> MessageRef {
        MessageRef::new(
            MailSourceKind::Imap,
            "imap.example.com",
            Some("INBOX".to_owned()),
            stable_id,
        )
    }

    fn html_email(subject: &str) -> Vec<u8> {
        format!(
            "From: Alice <alice@example.com>\r\nTo: Bob <bob@example.com>\r\nSubject: {subject}\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello</p>"
        )
        .into_bytes()
    }

    fn no_body_email() -> Vec<u8> {
        b"From: Alice <alice@example.com>\r\nSubject: Empty\r\n\r\n".to_vec()
    }

    fn page_store() -> Arc<PageStore> {
        page_store_with_config(Some(3), Duration::from_secs(60))
    }

    fn page_store_with_config(max_views: Option<u32>, ttl: Duration) -> Arc<PageStore> {
        Arc::new(PageStore::new(PageStoreConfig {
            page_ttl: ttl,
            page_max_views: max_views,
            remote_images: ViewerRemoteImages::Allow,
        }))
    }

    fn poll_service(
        mail_source: Arc<FakeMailSource>,
        page_store: Arc<PageStore>,
        state: Arc<RuntimeState>,
        callback_store: Arc<CallbackStore>,
        telegram: Arc<FakeTelegram>,
    ) -> std::result::Result<PollService, url::ParseError> {
        Ok(PollService::new(
            mail_source,
            page_store,
            state,
            callback_store,
            telegram,
            Url::parse("https://mail.example.com/view")?,
        ))
    }

    fn authorized_page(page_id: uuid::Uuid, mail_ref: MessageRef) -> AuthorizedPage {
        AuthorizedPage {
            id: page_id,
            created_at: Instant::now(),
            views: 0,
            mail_ref: Some(mail_ref),
        }
    }

    #[tokio::test]
    async fn mark_read_marks_source_hides_keyboard_and_cleans_mappings()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let page_id = uuid::Uuid::new_v4();
        let mail_ref = mail_ref();
        let telegram_ref = TelegramMessageRef::new(100, 200);
        let mail_source = Arc::new(FakeMailSource::default());
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        state.track_message(mail_ref.clone(), page_id, telegram_ref)?;
        let callback = callback_store.create(page_id, SecretString::from("secret".to_owned()))?;
        let service = MarkReadService::new(
            mail_source.clone(),
            Arc::clone(&state),
            Arc::clone(&callback_store),
            telegram.clone(),
            viewer_url_base,
        );

        let result = service
            .mark_read(authorized_page(page_id, mail_ref.clone()))
            .await?;

        assert!(result.keyboard_hidden);
        assert!(result.callback_deleted);
        assert_eq!(state.tracked_len()?, 0);
        assert!(callback_store.lookup(&callback.key)?.is_none());
        assert_eq!(
            mail_source.marked.lock().expect("marked lock").as_slice(),
            [mail_ref]
        );
        let edits = telegram.edits.lock().expect("edits lock");
        assert_eq!(
            edits.as_slice(),
            [(
                telegram_ref,
                Url::parse(&format!(
                    "https://mail.example.com/view?id={page_id}&token=secret"
                ))?
            )]
        );

        Ok(())
    }

    #[tokio::test]
    async fn mark_read_failure_keeps_mappings()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let page_id = uuid::Uuid::new_v4();
        let mail_ref = mail_ref();
        let telegram_ref = TelegramMessageRef::new(100, 200);
        let mail_source = Arc::new(FakeMailSource {
            fail_mark_read: true,
            ..FakeMailSource::default()
        });
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        let viewer_url_base = Url::parse("https://mail.example.com/view")?;

        state.track_message(mail_ref.clone(), page_id, telegram_ref)?;
        let callback = callback_store.create(page_id, SecretString::from("secret".to_owned()))?;
        let service = MarkReadService::new(
            mail_source,
            Arc::clone(&state),
            Arc::clone(&callback_store),
            telegram,
            viewer_url_base,
        );

        let result = service.mark_read(authorized_page(page_id, mail_ref)).await;

        assert!(result.is_err());
        assert_eq!(state.tracked_len()?, 1);
        assert!(callback_store.lookup(&callback.key)?.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn cleanup_expired_pages_removes_callbacks_and_tracking()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let page_store = page_store_with_config(None, Duration::from_millis(1));
        let page = page_store.create_page_with_options(
            "<p>Hello</p>",
            CreatePageOptions {
                mail_ref: Some(mail_ref.clone()),
            },
        )?;
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        state.track_message(mail_ref, page.id, TelegramMessageRef::new(100, 200))?;
        let callback = callback_store.create(page.id, page.token)?;
        let service = CleanupService::new(
            Arc::clone(&page_store),
            Arc::clone(&state),
            Arc::clone(&callback_store),
        );

        tokio::time::sleep(Duration::from_millis(5)).await;
        let stats = service.cleanup_expired_once()?;

        assert_eq!(
            stats,
            CleanupStats {
                deleted_pages: 1,
                callbacks_deleted: 1,
                tracked_removed: 1,
            }
        );
        assert!(page_store.is_empty()?);
        assert!(callback_store.lookup(&callback.key)?.is_none());
        assert_eq!(state.tracked_len()?, 0);

        Ok(())
    }

    #[tokio::test]
    async fn loop_interval_wait_exits_when_shutdown_is_cancelled() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();

        assert!(wait_for_interval_or_shutdown(Duration::from_secs(60), &shutdown).await);
    }

    #[tokio::test]
    async fn loop_interval_wait_continues_after_sleep() {
        let shutdown = CancellationToken::new();

        assert!(!wait_for_interval_or_shutdown(Duration::from_millis(1), &shutdown).await);
    }

    #[test]
    fn cleanup_service_removes_mappings_for_max_view_deleted_page()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let page_id = uuid::Uuid::new_v4();
        let mail_ref = mail_ref();
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        state.track_message(mail_ref.clone(), page_id, TelegramMessageRef::new(100, 200))?;
        let callback = callback_store.create(page_id, SecretString::from("secret".to_owned()))?;
        let service =
            CleanupService::new(page_store, Arc::clone(&state), Arc::clone(&callback_store));

        let result = service.page_deleted(DeletedPage {
            id: page_id,
            reason: DeletionReason::MaxViews,
            mail_ref: Some(mail_ref),
        })?;

        assert_eq!(
            result,
            PageDeletionResult {
                callback_deleted: true,
                tracked_removed: true,
            }
        );
        assert!(callback_store.lookup(&callback.key)?.is_none());
        assert_eq!(state.tracked_len()?, 0);

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_sends_new_email_and_tracks_state()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let mail_source = Arc::new(FakeMailSource::default());
        mail_source.set_unread(vec![mail_ref.clone()]);
        mail_source.set_raw(mail_ref.clone(), html_email("Hello"));
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        let service = poll_service(
            Arc::clone(&mail_source),
            Arc::clone(&page_store),
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(
            stats,
            PollCycleStats {
                unread: 1,
                processed: 1,
                ..PollCycleStats::default()
            }
        );
        assert!(state.is_processed(&mail_ref)?);
        let tracked = state.get_tracked(&mail_ref)?.expect("tracked message");
        assert_eq!(tracked.telegram_ref, TelegramMessageRef::new(100, 201));
        assert_eq!(callback_store.len()?, 1);
        assert!(callback_store.lookup_page(tracked.page_id)?.is_some());
        assert_eq!(page_store.len()?, 1);

        let sent = telegram.sent.lock().expect("sent lock");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].subject, "Hello");
        assert_eq!(sent[0].viewer_url.path(), "/view");
        assert!(
            sent[0]
                .viewer_url
                .query()
                .is_some_and(|query| { query.contains("id=") && query.contains("token=") })
        );
        assert!(sent[0].mark_callback_data.starts_with("mark:"));

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_auto_hides_externally_read_message_and_cleans_mappings()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let page_id = uuid::Uuid::new_v4();
        let telegram_ref = TelegramMessageRef::new(100, 200);
        let mail_source = Arc::new(FakeMailSource::default());
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        state.track_message(mail_ref.clone(), page_id, telegram_ref)?;
        let callback = callback_store.create(page_id, SecretString::from("secret".to_owned()))?;
        let service = poll_service(
            Arc::clone(&mail_source),
            page_store,
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(
            stats,
            PollCycleStats {
                auto_hidden: 1,
                ..PollCycleStats::default()
            }
        );
        assert_eq!(state.tracked_len()?, 0);
        assert!(callback_store.lookup(&callback.key)?.is_none());
        assert!(mail_source.fetched.lock().expect("fetched lock").is_empty());
        let edits = telegram.edits.lock().expect("edits lock");
        assert_eq!(
            edits.as_slice(),
            [(
                telegram_ref,
                Url::parse(&format!(
                    "https://mail.example.com/view?id={page_id}&token=secret"
                ))?
            )]
        );

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_keeps_mark_button_when_tracked_message_is_still_unseen()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let page_id = uuid::Uuid::new_v4();
        let telegram_ref = TelegramMessageRef::new(100, 200);
        let mail_source = Arc::new(FakeMailSource::default());
        mail_source.set_unread(vec![mail_ref.clone()]);
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        let _ = state.mark_processed(mail_ref.clone())?;
        state.track_message(mail_ref.clone(), page_id, telegram_ref)?;
        let callback = callback_store.create(page_id, SecretString::from("secret".to_owned()))?;
        let service = poll_service(
            Arc::clone(&mail_source),
            page_store,
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(stats.unread, 1);
        assert_eq!(stats.skipped_processed, 1);
        assert_eq!(stats.auto_hidden, 0);
        assert_eq!(state.tracked_len()?, 1);
        assert!(callback_store.lookup(&callback.key)?.is_some());
        assert!(telegram.edits.lock().expect("edits lock").is_empty());
        assert!(mail_source.fetched.lock().expect("fetched lock").is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_keeps_mappings_when_auto_hide_edit_fails()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let page_id = uuid::Uuid::new_v4();
        let telegram_ref = TelegramMessageRef::new(100, 200);
        let mail_source = Arc::new(FakeMailSource::default());
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram {
            fail_edit: true,
            ..FakeTelegram::default()
        });
        state.track_message(mail_ref.clone(), page_id, telegram_ref)?;
        let callback = callback_store.create(page_id, SecretString::from("secret".to_owned()))?;
        let service = poll_service(
            Arc::clone(&mail_source),
            page_store,
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(stats.auto_hidden, 0);
        assert_eq!(stats.auto_hide_errors, 1);
        assert_eq!(state.tracked_len()?, 1);
        assert!(callback_store.lookup(&callback.key)?.is_some());
        assert!(telegram.edits.lock().expect("edits lock").is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_removes_stale_tracking_without_callback_payload()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let page_id = uuid::Uuid::new_v4();
        let telegram_ref = TelegramMessageRef::new(100, 200);
        let mail_source = Arc::new(FakeMailSource::default());
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        state.track_message(mail_ref, page_id, telegram_ref)?;
        let service = poll_service(
            Arc::clone(&mail_source),
            page_store,
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(stats.stale_tracked, 1);
        assert_eq!(stats.auto_hidden, 0);
        assert_eq!(state.tracked_len()?, 0);
        assert!(callback_store.is_empty()?);
        assert!(telegram.edits.lock().expect("edits lock").is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_does_not_mark_processed_when_telegram_send_fails()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let mail_source = Arc::new(FakeMailSource::default());
        mail_source.set_unread(vec![mail_ref.clone()]);
        mail_source.set_raw(mail_ref.clone(), html_email("Hello"));
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram {
            edits: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            fail_send: true,
            fail_edit: false,
        });
        let service = poll_service(
            Arc::clone(&mail_source),
            Arc::clone(&page_store),
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(stats.unread, 1);
        assert_eq!(stats.telegram_errors, 1);
        assert!(!state.is_processed(&mail_ref)?);
        assert_eq!(state.tracked_len()?, 0);
        assert!(callback_store.is_empty()?);
        assert!(page_store.is_empty()?);

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_skips_already_processed_without_fetch()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref();
        let mail_source = Arc::new(FakeMailSource::default());
        mail_source.set_unread(vec![mail_ref.clone()]);
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        let service = poll_service(
            Arc::clone(&mail_source),
            page_store,
            Arc::clone(&state),
            callback_store,
            telegram,
        )?;
        let _ = state.mark_processed(mail_ref)?;

        let stats = service.poll_once().await?;

        assert_eq!(stats.unread, 1);
        assert_eq!(stats.skipped_processed, 1);
        assert!(mail_source.fetched.lock().expect("fetched lock").is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn poll_once_marks_no_body_email_processed_without_sending()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mail_ref = mail_ref_with_id("43");
        let mail_source = Arc::new(FakeMailSource::default());
        mail_source.set_unread(vec![mail_ref.clone()]);
        mail_source.set_raw(mail_ref.clone(), no_body_email());
        let page_store = page_store();
        let state = Arc::new(RuntimeState::new());
        let callback_store = Arc::new(CallbackStore::new());
        let telegram = Arc::new(FakeTelegram::default());
        let service = poll_service(
            Arc::clone(&mail_source),
            Arc::clone(&page_store),
            Arc::clone(&state),
            Arc::clone(&callback_store),
            Arc::clone(&telegram),
        )?;

        let stats = service.poll_once().await?;

        assert_eq!(stats.unread, 1);
        assert_eq!(stats.skipped_no_body, 1);
        assert!(state.is_processed(&mail_ref)?);
        assert!(telegram.sent.lock().expect("sent lock").is_empty());
        assert!(callback_store.is_empty()?);
        assert!(page_store.is_empty()?);

        Ok(())
    }
}
