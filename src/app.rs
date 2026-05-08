use std::sync::Arc;

use crate::{
    config::{Config, MailSourceConfig},
    error::{Error, Result},
    shutdown,
    viewer::{
        http::{self, NoopMarkReadHandler, ViewerHttpState},
        store::{PageStore, PageStoreConfig},
    },
};
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

pub fn init_tracing(log_filter: &str) -> Result<()> {
    let env_filter = EnvFilter::try_new(log_filter)?;

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .try_init()
        .map_err(|error| Error::TracingInit(error.to_string()))?;

    Ok(())
}

pub async fn run(config: Config) -> Result<()> {
    log_startup(&config);

    let page_store = Arc::new(PageStore::new(PageStoreConfig::from(&config.viewer)));
    let viewer_state = ViewerHttpState::new(
        page_store,
        config.viewer.remote_images,
        Arc::new(NoopMarkReadHandler),
    );
    let bind_addr = normalize_http_addr(&config.http.addr);
    let listener = TcpListener::bind(&bind_addr).await?;

    info!(
        bind_addr = %bind_addr,
        "viewer HTTP listener started; mail polling and telegram loops are not active yet"
    );

    axum::serve(listener, http::router(viewer_state))
        .with_graceful_shutdown(async {
            if let Err(error) = shutdown::wait_for_signal().await {
                tracing::error!(%error, "failed while waiting for shutdown signal");
            }
        })
        .await?;

    info!("shutdown complete");

    Ok(())
}

fn normalize_http_addr(addr: &str) -> String {
    addr.strip_prefix(':')
        .map(|port| format!("0.0.0.0:{port}"))
        .unwrap_or_else(|| addr.to_owned())
}

fn log_startup(config: &Config) {
    info!(
        mail_source = %config.mail_source.kind(),
        http_addr = %config.http.addr,
        viewer_url_base = %config.viewer.url_base,
        viewer_remote_images = ?config.viewer.remote_images,
        "starting mailpuff"
    );

    match &config.mail_source {
        MailSourceConfig::Imap(imap) => {
            if !imap.use_tls {
                warn!("IMAP_TLS=false is legacy compatibility mode; plaintext IMAP is not enabled");
            }
            if imap.accept_invalid_certs {
                warn!(
                    "IMAP_ACCEPT_INVALID_CERTS=true disables certificate validation for IMAP TLS"
                );
            }
        }
    }
}
