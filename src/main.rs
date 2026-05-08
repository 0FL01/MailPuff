use mailpuff::{app, config::Config, error::Result};

#[tokio::main]
async fn main() -> Result<()> {
    install_rustls_crypto_provider();

    let config = Config::from_env()?;
    app::init_tracing(&config.log_filter)?;
    app::run(config).await
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
