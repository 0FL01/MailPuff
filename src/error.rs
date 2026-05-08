use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    #[error(transparent)]
    EmailParse(#[from] crate::email::parser::EmailParseError),

    #[error(transparent)]
    ImapSource(#[from] crate::mail_source::imap::ImapSourceError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid RUST_LOG filter: {0}")]
    LogFilter(#[from] tracing_subscriber::filter::ParseError),

    #[error("tracing subscriber initialization failed: {0}")]
    TracingInit(String),

    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
}
