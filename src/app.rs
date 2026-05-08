use crate::{
    config::{Config, MailSourceConfig},
    error::{Error, Result},
    shutdown,
};
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

    info!(
        "rust phase 1 skeleton is running; mail polling, viewer, and telegram loops are not active yet"
    );
    shutdown::wait_for_signal().await?;
    info!("shutdown signal received");

    Ok(())
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
