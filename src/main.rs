use mailpuff::{app, config::Config, error::Result};

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    app::init_tracing(&config.log_filter)?;
    app::run(config).await
}
