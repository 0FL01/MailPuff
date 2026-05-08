use std::sync::Arc;

use crate::{
    config::{Config, MailSourceConfig},
    error::{Error, Result},
    mail_source::{MailSource, imap::ImapSource},
    orchestration::MarkReadService,
    shutdown,
    state::RuntimeState,
    telegram::{
        TelegramBot,
        callbacks::{CallbackStore, CallbackTelegramApi},
    },
    viewer::{
        http::{self, MarkReadHandler, ViewerHttpState},
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
    let callback_store = Arc::new(CallbackStore::new());
    let runtime_state = Arc::new(RuntimeState::new());
    let mail_source = build_mail_source(&config.mail_source);
    let telegram_bot = Arc::new(TelegramBot::new(&config.telegram));
    let telegram_api: Arc<dyn CallbackTelegramApi> = telegram_bot.clone();
    let mark_read_handler: Arc<dyn MarkReadHandler> = Arc::new(MarkReadService::new(
        Arc::clone(&mail_source),
        Arc::clone(&runtime_state),
        Arc::clone(&callback_store),
        telegram_api,
        config.viewer.url_base.clone(),
    ));
    let viewer_state = ViewerHttpState::new(
        Arc::clone(&page_store),
        config.viewer.remote_images,
        Arc::clone(&mark_read_handler),
    );
    let telegram_task = tokio::spawn({
        let telegram_bot = Arc::clone(&telegram_bot);
        let page_store = Arc::clone(&page_store);
        let callback_store = Arc::clone(&callback_store);
        let mark_read_handler = Arc::clone(&mark_read_handler);
        let viewer_url_base = config.viewer.url_base.clone();

        async move {
            info!("telegram callback loop started");
            crate::telegram::callbacks::run_callback_loop(
                telegram_bot,
                page_store,
                callback_store,
                mark_read_handler,
                viewer_url_base,
            )
            .await;
        }
    });
    let bind_addr = normalize_http_addr(&config.http.addr);
    let listener = TcpListener::bind(&bind_addr).await?;

    info!(
        bind_addr = %bind_addr,
        "viewer HTTP listener started; mail polling is not active yet"
    );

    axum::serve(listener, http::router(viewer_state))
        .with_graceful_shutdown(async {
            if let Err(error) = shutdown::wait_for_signal().await {
                tracing::error!(%error, "failed while waiting for shutdown signal");
            }
        })
        .await?;

    telegram_task.abort();
    if let Err(error) = telegram_task.await
        && !error.is_cancelled()
    {
        tracing::error!(%error, "telegram callback task failed during shutdown");
    }

    info!("shutdown complete");

    Ok(())
}

fn normalize_http_addr(addr: &str) -> String {
    addr.strip_prefix(':')
        .map(|port| format!("0.0.0.0:{port}"))
        .unwrap_or_else(|| addr.to_owned())
}

fn build_mail_source(config: &MailSourceConfig) -> Arc<dyn MailSource> {
    match config {
        MailSourceConfig::Imap(imap) => Arc::new(ImapSource::new(imap.clone())),
    }
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
