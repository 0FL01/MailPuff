use std::sync::Arc;

use crate::{
    config::{Config, MailSourceConfig},
    error::{Error, Result},
    mail_source::{MailSource, imap::ImapSource},
    orchestration::{
        CleanupService, EmailTelegramApi, MarkReadService, PollService, cleanup_interval,
        run_cleanup_loop, run_poll_loop,
    },
    shutdown,
    state::RuntimeState,
    telegram::{
        TelegramBot,
        callbacks::{CallbackStore, CallbackTelegramApi},
    },
    viewer::{
        http::{self, MarkReadHandler, PageDeletionHandler, ViewerHttpState},
        store::{PageStore, PageStoreConfig},
    },
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
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
    let telegram_sender: Arc<dyn EmailTelegramApi> = telegram_bot.clone();
    let cleanup_service = Arc::new(CleanupService::new(
        Arc::clone(&page_store),
        Arc::clone(&runtime_state),
        Arc::clone(&callback_store),
    ));
    let page_deleted_handler: Arc<dyn PageDeletionHandler> = cleanup_service.clone();
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
        page_deleted_handler,
        mail_mark_seen_on_first_view(&config.mail_source),
    );
    let shutdown_token = CancellationToken::new();
    let telegram_task = tokio::spawn({
        let telegram_bot = Arc::clone(&telegram_bot);
        let page_store = Arc::clone(&page_store);
        let callback_store = Arc::clone(&callback_store);
        let mark_read_handler = Arc::clone(&mark_read_handler);
        let viewer_url_base = config.viewer.url_base.clone();
        let shutdown = shutdown_token.child_token();

        async move {
            info!("telegram callback loop started");
            crate::telegram::callbacks::run_callback_loop(
                telegram_bot,
                page_store,
                callback_store,
                mark_read_handler,
                viewer_url_base,
                shutdown,
            )
            .await;
        }
    });
    let poll_task = tokio::spawn({
        let poll_service = PollService::new(
            Arc::clone(&mail_source),
            Arc::clone(&page_store),
            Arc::clone(&runtime_state),
            Arc::clone(&callback_store),
            telegram_sender,
            config.viewer.url_base.clone(),
        );
        let poll_interval = mail_poll_interval(&config.mail_source);
        let shutdown = shutdown_token.child_token();

        async move {
            info!(?poll_interval, "mail poll loop started");
            run_poll_loop(poll_service, poll_interval, shutdown).await;
        }
    });
    let cleanup_task = tokio::spawn({
        let cleanup_service = Arc::clone(&cleanup_service);
        let cleanup_interval = cleanup_interval(config.viewer.page_ttl);
        let shutdown = shutdown_token.child_token();

        async move {
            info!(?cleanup_interval, "viewer cleanup loop started");
            run_cleanup_loop(cleanup_service, cleanup_interval, shutdown).await;
        }
    });
    let bind_addr = normalize_http_addr(&config.http.addr);
    let listener = TcpListener::bind(&bind_addr).await?;

    info!(
        bind_addr = %bind_addr,
        "viewer HTTP listener started"
    );

    let http_shutdown = shutdown_token.clone();
    let server = std::future::IntoFuture::into_future(
        axum::serve(listener, http::router(viewer_state))
            .with_graceful_shutdown(http_shutdown.cancelled_owned()),
    );
    tokio::pin!(server);

    let server_result = tokio::select! {
        result = &mut server => result,
        signal_result = shutdown::wait_for_signal() => {
            if let Err(error) = signal_result {
                tracing::error!(%error, "failed while waiting for shutdown signal");
            }
            shutdown_token.cancel();
            server.as_mut().await
        }
    };

    shutdown_token.cancel();
    await_task("telegram callback", telegram_task).await;
    await_task("mail poll", poll_task).await;
    await_task("viewer cleanup", cleanup_task).await;

    server_result?;

    info!("shutdown complete");

    Ok(())
}

async fn await_task(name: &'static str, task: JoinHandle<()>) {
    match task.await {
        Ok(()) => info!(task = name, "background task stopped"),
        Err(error) => {
            tracing::error!(task = name, %error, "background task failed during shutdown")
        }
    }
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

fn mail_poll_interval(config: &MailSourceConfig) -> std::time::Duration {
    match config {
        MailSourceConfig::Imap(imap) => imap.poll_interval,
    }
}

fn mail_mark_seen_on_first_view(config: &MailSourceConfig) -> bool {
    match config {
        MailSourceConfig::Imap(imap) => imap.mark_seen,
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
