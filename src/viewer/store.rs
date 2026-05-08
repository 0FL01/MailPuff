use std::{collections::HashMap, fmt, sync::RwLock, time::Duration, time::Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use secrecy::{ExposeSecret, SecretString};
use subtle::ConstantTimeEq;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::{ViewerConfig, ViewerRemoteImages},
    mail_source::MessageRef,
    viewer::sanitize::HtmlSanitizer,
};

#[derive(Debug, Clone, Copy)]
pub struct PageStoreConfig {
    pub page_ttl: Duration,
    pub page_max_views: Option<u32>,
    pub remote_images: ViewerRemoteImages,
}

impl From<&ViewerConfig> for PageStoreConfig {
    fn from(config: &ViewerConfig) -> Self {
        Self {
            page_ttl: config.page_ttl,
            page_max_views: config.page_max_views,
            remote_images: config.remote_images,
        }
    }
}

#[derive(Debug)]
pub struct PageStore {
    pages: RwLock<HashMap<Uuid, Page>>,
    config: PageStoreConfig,
    sanitizer: HtmlSanitizer,
}

impl PageStore {
    #[must_use]
    pub fn new(config: PageStoreConfig) -> Self {
        Self {
            pages: RwLock::new(HashMap::new()),
            config,
            sanitizer: HtmlSanitizer::new(config.remote_images),
        }
    }

    pub fn create_page(&self, html: &str) -> Result<CreatedPage, StoreError> {
        self.create_page_with_options(html, CreatePageOptions::default())
    }

    pub fn create_page_with_options(
        &self,
        html: &str,
        options: CreatePageOptions,
    ) -> Result<CreatedPage, StoreError> {
        if html.trim().is_empty() {
            return Err(StoreError::EmptyHtml);
        }

        let sanitized = self.sanitizer.sanitize(html);
        if sanitized.trim().is_empty() {
            return Err(StoreError::EmptyHtmlAfterSanitization);
        }

        let id = Uuid::new_v4();
        let token = generate_token()?;
        let now = Instant::now();
        let expires_at = now + self.config.page_ttl;

        let page = Page {
            id,
            token: SecretString::from(token.clone()),
            html: sanitized,
            created_at: now,
            expires_at,
            max_views: self.config.page_max_views,
            views: 0,
            mail_ref: options.mail_ref,
        };

        let mut pages = self.pages.write().map_err(|_| StoreError::LockPoisoned)?;
        pages.insert(id, page);

        Ok(CreatedPage {
            id,
            token: SecretString::from(token),
            created_at: now,
            expires_at,
        })
    }

    pub fn view(&self, id: Uuid, token: &str) -> Result<PageAccess<ViewedPage>, StoreError> {
        let now = Instant::now();
        let mut pages = self.pages.write().map_err(|_| StoreError::LockPoisoned)?;

        let Some(page) = pages.get_mut(&id) else {
            return Ok(PageAccess::Denied(AccessDenied::NotFound));
        };

        if !secure_token_eq(page.token.expose_secret(), token) {
            return Ok(PageAccess::Denied(AccessDenied::InvalidToken));
        }

        if page.is_expired(now) {
            pages.remove(&id);
            return Ok(PageAccess::Denied(AccessDenied::Expired));
        }

        let first_view = page.views == 0;
        page.views += 1;
        let deleted_after_view = page
            .max_views
            .is_some_and(|max_views| page.views >= max_views)
            .then_some(DeletionReason::MaxViews);

        let viewed = ViewedPage {
            id: page.id,
            html: page.html.clone(),
            created_at: page.created_at,
            first_view,
            views: page.views,
            deleted_after_view,
            mail_ref: page.mail_ref.clone(),
        };

        if deleted_after_view.is_some() {
            pages.remove(&id);
        }

        Ok(PageAccess::Granted(viewed))
    }

    pub fn authorize(
        &self,
        id: Uuid,
        token: &str,
    ) -> Result<PageAccess<AuthorizedPage>, StoreError> {
        let now = Instant::now();
        let mut pages = self.pages.write().map_err(|_| StoreError::LockPoisoned)?;

        let Some(page) = pages.get(&id) else {
            return Ok(PageAccess::Denied(AccessDenied::NotFound));
        };

        if !secure_token_eq(page.token.expose_secret(), token) {
            return Ok(PageAccess::Denied(AccessDenied::InvalidToken));
        }

        if page.is_expired(now) {
            pages.remove(&id);
            return Ok(PageAccess::Denied(AccessDenied::Expired));
        }

        Ok(PageAccess::Granted(AuthorizedPage {
            id: page.id,
            created_at: page.created_at,
            views: page.views,
            mail_ref: page.mail_ref.clone(),
        }))
    }

    pub fn delete(&self, id: Uuid) -> Result<Option<DeletedPage>, StoreError> {
        let mut pages = self.pages.write().map_err(|_| StoreError::LockPoisoned)?;

        Ok(pages
            .remove(&id)
            .map(|page| DeletedPage::from_page(page, DeletionReason::Manual)))
    }

    pub fn cleanup_expired(&self) -> Result<Vec<DeletedPage>, StoreError> {
        let now = Instant::now();
        let mut pages = self.pages.write().map_err(|_| StoreError::LockPoisoned)?;
        let expired_ids = pages
            .iter()
            .filter_map(|(id, page)| page.is_expired(now).then_some(*id))
            .collect::<Vec<_>>();

        Ok(expired_ids
            .into_iter()
            .filter_map(|id| pages.remove(&id))
            .map(|page| DeletedPage::from_page(page, DeletionReason::Expired))
            .collect())
    }

    pub fn len(&self) -> Result<usize, StoreError> {
        Ok(self
            .pages
            .read()
            .map_err(|_| StoreError::LockPoisoned)?
            .len())
    }

    pub fn is_empty(&self) -> Result<bool, StoreError> {
        Ok(self.len()? == 0)
    }
}

#[derive(Debug, Default, Clone)]
pub struct CreatePageOptions {
    pub mail_ref: Option<MessageRef>,
}

struct Page {
    id: Uuid,
    token: SecretString,
    html: String,
    created_at: Instant,
    expires_at: Instant,
    max_views: Option<u32>,
    views: u32,
    mail_ref: Option<MessageRef>,
}

impl fmt::Debug for Page {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Page")
            .field("id", &self.id)
            .field("token", &"[REDACTED]")
            .field("html_len", &self.html.len())
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("max_views", &self.max_views)
            .field("views", &self.views)
            .field("mail_ref", &self.mail_ref)
            .finish()
    }
}

impl Page {
    fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

#[derive(Clone)]
pub struct CreatedPage {
    pub id: Uuid,
    pub token: SecretString,
    pub created_at: Instant,
    pub expires_at: Instant,
}

impl fmt::Debug for CreatedPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatedPage")
            .field("id", &self.id)
            .field("token", &"[REDACTED]")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewedPage {
    pub id: Uuid,
    pub html: String,
    pub created_at: Instant,
    pub first_view: bool,
    pub views: u32,
    pub deleted_after_view: Option<DeletionReason>,
    pub mail_ref: Option<MessageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedPage {
    pub id: Uuid,
    pub created_at: Instant,
    pub views: u32,
    pub mail_ref: Option<MessageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletedPage {
    pub id: Uuid,
    pub reason: DeletionReason,
    pub mail_ref: Option<MessageRef>,
}

impl DeletedPage {
    fn from_page(page: Page, reason: DeletionReason) -> Self {
        Self {
            id: page.id,
            reason,
            mail_ref: page.mail_ref,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionReason {
    Expired,
    MaxViews,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAccess<T> {
    Granted(T),
    Denied(AccessDenied),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessDenied {
    NotFound,
    InvalidToken,
    Expired,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("empty html")]
    EmptyHtml,

    #[error("empty html after sanitization")]
    EmptyHtmlAfterSanitization,

    #[error("token generation failed: {0}")]
    TokenGeneration(String),

    #[error("page store lock poisoned")]
    LockPoisoned,
}

fn generate_token() -> Result<String, StoreError> {
    let mut bytes = [0_u8; 18];
    getrandom::fill(&mut bytes).map_err(|error| StoreError::TokenGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn secure_token_eq(expected: &str, actual: &str) -> bool {
    expected.as_bytes().ct_eq(actual.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use secrecy::ExposeSecret;

    use super::*;

    fn store_config(max_views: Option<u32>, ttl: Duration) -> PageStoreConfig {
        PageStoreConfig {
            page_ttl: ttl,
            page_max_views: max_views,
            remote_images: ViewerRemoteImages::Allow,
        }
    }

    #[test]
    fn creates_and_authorizes_page() -> Result<(), StoreError> {
        let store = PageStore::new(store_config(Some(3), Duration::from_secs(60)));
        let page = store.create_page("<p>Hello</p>")?;

        assert!(matches!(
            store.authorize(page.id, page.token.expose_secret())?,
            PageAccess::Granted(_)
        ));
        assert_eq!(
            store.authorize(page.id, "wrong-token")?,
            PageAccess::Denied(AccessDenied::InvalidToken)
        );

        Ok(())
    }

    #[test]
    fn detects_first_view_without_incrementing_authorize() -> Result<(), StoreError> {
        let store = PageStore::new(store_config(None, Duration::from_secs(60)));
        let page = store.create_page("<p>Hello</p>")?;

        assert!(matches!(
            store.authorize(page.id, page.token.expose_secret())?,
            PageAccess::Granted(AuthorizedPage { views: 0, .. })
        ));
        assert!(matches!(
            store.view(page.id, page.token.expose_secret())?,
            PageAccess::Granted(ViewedPage {
                first_view: true,
                views: 1,
                ..
            })
        ));
        assert!(matches!(
            store.view(page.id, page.token.expose_secret())?,
            PageAccess::Granted(ViewedPage {
                first_view: false,
                views: 2,
                ..
            })
        ));

        Ok(())
    }

    #[test]
    fn max_views_allows_exact_limit_then_deletes() -> Result<(), StoreError> {
        let store = PageStore::new(store_config(Some(3), Duration::from_secs(60)));
        let page = store.create_page("<p>Hello</p>")?;

        for expected_views in 1..=3 {
            assert!(matches!(
                store.view(page.id, page.token.expose_secret())?,
                PageAccess::Granted(ViewedPage { views, .. }) if views == expected_views
            ));
        }

        assert_eq!(
            store.view(page.id, page.token.expose_secret())?,
            PageAccess::Denied(AccessDenied::NotFound)
        );

        Ok(())
    }

    #[test]
    fn expires_pages() -> Result<(), StoreError> {
        let store = PageStore::new(store_config(None, Duration::from_millis(1)));
        let page = store.create_page("<p>Hello</p>")?;

        thread::sleep(Duration::from_millis(5));

        assert_eq!(
            store.authorize(page.id, page.token.expose_secret())?,
            PageAccess::Denied(AccessDenied::Expired)
        );
        assert_eq!(store.len()?, 0);

        Ok(())
    }

    #[test]
    fn cleanup_expired_returns_deleted_pages() -> Result<(), StoreError> {
        let store = PageStore::new(store_config(None, Duration::from_millis(1)));
        let page = store.create_page("<p>Hello</p>")?;

        thread::sleep(Duration::from_millis(5));

        let deleted = store.cleanup_expired()?;
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].id, page.id);
        assert_eq!(deleted[0].reason, DeletionReason::Expired);
        assert!(store.is_empty()?);

        Ok(())
    }
}
