use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use thiserror::Error;
use tracing::error;
use uuid::Uuid;

use crate::{
    config::ViewerRemoteImages,
    viewer::store::{AuthorizedPage, PageAccess, PageStore},
};

#[derive(Clone)]
pub struct ViewerHttpState {
    store: Arc<PageStore>,
    remote_images: ViewerRemoteImages,
    mark_read: Arc<dyn MarkReadHandler>,
}

impl ViewerHttpState {
    #[must_use]
    pub fn new(
        store: Arc<PageStore>,
        remote_images: ViewerRemoteImages,
        mark_read: Arc<dyn MarkReadHandler>,
    ) -> Self {
        Self {
            store,
            remote_images,
            mark_read,
        }
    }
}

pub fn router(state: ViewerHttpState) -> Router {
    Router::new()
        .route("/view", get(view))
        .route("/mark_read", get(mark_read))
        .with_state(state)
}

#[async_trait]
pub trait MarkReadHandler: Send + Sync + 'static {
    async fn mark_read(&self, page: AuthorizedPage) -> Result<(), MarkReadError>;
}

#[derive(Debug, Default)]
pub struct NoopMarkReadHandler;

#[async_trait]
impl MarkReadHandler for NoopMarkReadHandler {
    async fn mark_read(&self, _page: AuthorizedPage) -> Result<(), MarkReadError> {
        Err(MarkReadError::NotConfigured)
    }
}

#[derive(Debug, Error)]
pub enum MarkReadError {
    #[error("mark-read backend is not configured")]
    NotConfigured,

    #[error("mark-read backend failed: {0}")]
    Backend(String),
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    id: Option<String>,
    token: Option<String>,
}

async fn view(State(state): State<ViewerHttpState>, Query(query): Query<PageQuery>) -> Response {
    let Some((id, token)) = parse_page_query(query) else {
        return not_found();
    };

    match state.store.view(id, &token) {
        Ok(PageAccess::Granted(page)) => html_response(page.html, state.remote_images),
        Ok(PageAccess::Denied(_)) => not_found(),
        Err(error) => {
            error!(%id, %error, "viewer store failed while serving page");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn mark_read(
    State(state): State<ViewerHttpState>,
    Query(query): Query<PageQuery>,
) -> Response {
    let Some((id, token)) = parse_page_query(query) else {
        return not_found();
    };

    let page = match state.store.authorize(id, &token) {
        Ok(PageAccess::Granted(page)) => page,
        Ok(PageAccess::Denied(_)) => return not_found(),
        Err(error) => {
            error!(%id, %error, "viewer store failed while authorizing mark-read");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    if page.mail_ref.is_none() {
        return not_found();
    }

    match state.mark_read.mark_read(page).await {
        Ok(()) => plain_text(StatusCode::OK, "OK"),
        Err(MarkReadError::NotConfigured) => StatusCode::NOT_IMPLEMENTED.into_response(),
        Err(error) => {
            error!(%id, %error, "mark-read backend failed");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

fn parse_page_query(query: PageQuery) -> Option<(Uuid, String)> {
    let id = Uuid::parse_str(query.id.as_deref()?).ok()?;
    let token = query.token?.trim().to_owned();

    (!token.is_empty()).then_some((id, token))
}

fn html_response(html: String, remote_images: ViewerRemoteImages) -> Response {
    let mut response = Html(html).into_response();
    let headers = response.headers_mut();

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(content_security_policy(remote_images)),
    );

    response
}

fn plain_text(status: StatusCode, body: &'static str) -> Response {
    let mut response = (status, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

const fn content_security_policy(remote_images: ViewerRemoteImages) -> &'static str {
    match remote_images {
        ViewerRemoteImages::Allow => {
            "default-src 'none'; img-src http: https: data: cid:; style-src 'unsafe-inline'; font-src data: https:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        }
        ViewerRemoteImages::Block => {
            "default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use secrecy::ExposeSecret;
    use tower::ServiceExt;

    use crate::{
        mail_source::{MailSourceKind, MessageRef},
        viewer::store::{CreatePageOptions, PageStoreConfig},
    };

    use super::*;

    #[derive(Debug)]
    struct OkMarkReadHandler;

    #[async_trait]
    impl MarkReadHandler for OkMarkReadHandler {
        async fn mark_read(&self, _page: AuthorizedPage) -> Result<(), MarkReadError> {
            Ok(())
        }
    }

    fn page_store(max_views: Option<u32>) -> Arc<PageStore> {
        Arc::new(PageStore::new(PageStoreConfig {
            page_ttl: Duration::from_secs(60),
            page_max_views: max_views,
            remote_images: ViewerRemoteImages::Allow,
        }))
    }

    fn request(uri: String) -> Result<Request<Body>, axum::http::Error> {
        Request::builder().uri(uri).body(Body::empty())
    }

    async fn send(app: Router, uri: String) -> Result<Response, Box<dyn std::error::Error>> {
        let request = request(uri)?;
        let response = match app.oneshot(request).await {
            Ok(response) => response,
            Err(error) => match error {},
        };

        Ok(response)
    }

    async fn body_string(response: Response) -> Result<String, Box<dyn std::error::Error>> {
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(String::from_utf8(bytes.to_vec())?)
    }

    #[tokio::test]
    async fn view_returns_sanitized_html_and_security_headers()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = page_store(Some(3));
        let page = store.create_page("<p onclick=\"x()\">Hello</p><script>bad()</script>")?;
        let app = router(ViewerHttpState::new(
            store,
            ViewerRemoteImages::Allow,
            Arc::new(NoopMarkReadHandler),
        ));

        let response = send(
            app,
            format!("/view?id={}&token={}", page.id, page.token.expose_secret()),
        )
        .await?;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::X_CONTENT_TYPE_OPTIONS),
            Some(&HeaderValue::from_static("nosniff"))
        );
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static("referrer-policy")),
            Some(&HeaderValue::from_static("no-referrer"))
        );

        let body = body_string(response).await?;
        assert!(body.contains("<p>Hello</p>"));
        assert!(!body.contains("onclick"));
        assert!(!body.contains("script"));

        Ok(())
    }

    #[tokio::test]
    async fn invalid_view_token_returns_404() -> Result<(), Box<dyn std::error::Error>> {
        let store = page_store(Some(3));
        let page = store.create_page("<p>Hello</p>")?;
        let app = router(ViewerHttpState::new(
            store,
            ViewerRemoteImages::Allow,
            Arc::new(NoopMarkReadHandler),
        ));

        let response = send(app, format!("/view?id={}&token=wrong", page.id)).await?;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        Ok(())
    }

    #[tokio::test]
    async fn mark_read_authorizes_without_incrementing_views()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = page_store(Some(1));
        let mail_ref = MessageRef::new(
            MailSourceKind::Imap,
            "imap.example.com",
            Some("INBOX".to_owned()),
            "42",
        );
        let page = store.create_page_with_options(
            "<p>Hello</p>",
            CreatePageOptions {
                mail_ref: Some(mail_ref),
            },
        )?;
        let app = router(ViewerHttpState::new(
            Arc::clone(&store),
            ViewerRemoteImages::Allow,
            Arc::new(OkMarkReadHandler),
        ));

        let mark_response = send(
            app.clone(),
            format!(
                "/mark_read?id={}&token={}",
                page.id,
                page.token.expose_secret()
            ),
        )
        .await?;
        assert_eq!(mark_response.status(), StatusCode::OK);

        let view_response = send(
            app,
            format!("/view?id={}&token={}", page.id, page.token.expose_secret()),
        )
        .await?;
        assert_eq!(view_response.status(), StatusCode::OK);

        Ok(())
    }
}
