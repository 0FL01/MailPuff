use std::{fmt, sync::Arc};

use async_trait::async_trait;
use secrecy::ExposeSecret;
use url::Url;

use crate::{
    mail_source::MailSource,
    state::RuntimeState,
    telegram::{
        CallbackStore,
        callbacks::{CallbackTelegramApi, build_viewer_url},
    },
    viewer::{
        http::{MarkReadError, MarkReadHandler, MarkReadResult},
        store::AuthorizedPage,
    },
};

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
    use std::{sync::Mutex, time::Instant};

    use crate::{
        error::{Error, Result},
        mail_source::{MailSourceCapabilities, MailSourceKind, MessageRef, RawEmail},
        telegram::{TelegramError, TelegramMessageRef},
    };

    use secrecy::SecretString;

    use super::*;

    #[derive(Debug, Default)]
    struct FakeMailSource {
        marked: Mutex<Vec<MessageRef>>,
        fail_mark_read: bool,
    }

    #[async_trait]
    impl MailSource for FakeMailSource {
        async fn list_unread(&self) -> Result<Vec<MessageRef>> {
            Ok(Vec::new())
        }

        async fn fetch(&self, message: &MessageRef) -> Result<RawEmail> {
            Ok(RawEmail::new(Vec::new(), message.clone()))
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
            self.edits
                .lock()
                .expect("edits lock")
                .push((message, viewer_url));
            Ok(())
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
            marked: Mutex::new(Vec::new()),
            fail_mark_read: true,
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
}
