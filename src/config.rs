use std::{collections::BTreeMap, fmt, time::Duration};

use secrecy::SecretString;
use thiserror::Error;
use url::Url;

use crate::mail_source::MailSourceKind;

const DEFAULT_MAIL_SOURCE: &str = "imap";
const DEFAULT_IMAP_PORT: &str = "993";
const DEFAULT_IMAP_TLS: &str = "true";
const DEFAULT_IMAP_ACCEPT_INVALID_CERTS: &str = "false";
const DEFAULT_IMAP_MAILBOX: &str = "INBOX";
const DEFAULT_IMAP_POLL_INTERVAL: &str = "60s";
const DEFAULT_IMAP_FORCE_RECONNECT: &str = "60s";
const DEFAULT_IMAP_MARK_SEEN: &str = "false";
const DEFAULT_HTTP_ADDR: &str = ":8080";
const DEFAULT_VIEWER_PAGE_TTL: &str = "48h";
const DEFAULT_VIEWER_PAGE_MAX_VIEWS: &str = "3";
const DEFAULT_VIEWER_REMOTE_IMAGES: &str = "allow";
const DEFAULT_RUST_LOG: &str = "info";

#[derive(Debug, Clone)]
pub struct Config {
    pub mail_source: MailSourceConfig,
    pub telegram: TelegramConfig,
    pub http: HttpConfig,
    pub viewer: ViewerConfig,
    pub log_filter: String,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let vars = std::env::vars().collect();
        Self::from_env_map(&vars)
    }

    pub fn from_env_map(vars: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let mail_source_value = optional_string(vars, "MAIL_SOURCE", DEFAULT_MAIL_SOURCE)?;
        let mail_source_kind = parse_mail_source_kind("MAIL_SOURCE", &mail_source_value)?;

        let mail_source = match mail_source_kind {
            MailSourceKind::Imap => MailSourceConfig::Imap(ImapConfig::from_env_map(vars)?),
            MailSourceKind::ProtonCustom => {
                return Err(ConfigError::UnsupportedMailSource {
                    mail_source: mail_source_value,
                    reason: "reserved for a future custom Proton Mail backend",
                });
            }
        };

        Ok(Self {
            mail_source,
            telegram: TelegramConfig::from_env_map(vars)?,
            http: HttpConfig::from_env_map(vars)?,
            viewer: ViewerConfig::from_env_map(vars)?,
            log_filter: optional_string(vars, "RUST_LOG", DEFAULT_RUST_LOG)?,
        })
    }
}

#[derive(Debug, Clone)]
pub enum MailSourceConfig {
    Imap(ImapConfig),
}

impl MailSourceConfig {
    #[must_use]
    pub const fn kind(&self) -> MailSourceKind {
        match self {
            Self::Imap(_) => MailSourceKind::Imap,
        }
    }
}

#[derive(Clone)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: SecretString,
    pub use_tls: bool,
    pub accept_invalid_certs: bool,
    pub mailbox: String,
    pub poll_interval: Duration,
    pub force_reconnect: Duration,
    pub mark_seen: bool,
}

impl ImapConfig {
    fn from_env_map(vars: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        Ok(Self {
            host: required_string(vars, "IMAP_HOST")?,
            port: parse_u16(
                "IMAP_PORT",
                &optional_string(vars, "IMAP_PORT", DEFAULT_IMAP_PORT)?,
            )?,
            username: required_string(vars, "IMAP_USERNAME")?,
            password: SecretString::from(required_string(vars, "IMAP_PASSWORD")?),
            use_tls: parse_bool(
                "IMAP_TLS",
                &optional_string(vars, "IMAP_TLS", DEFAULT_IMAP_TLS)?,
            )?,
            accept_invalid_certs: parse_bool(
                "IMAP_ACCEPT_INVALID_CERTS",
                &optional_string(
                    vars,
                    "IMAP_ACCEPT_INVALID_CERTS",
                    DEFAULT_IMAP_ACCEPT_INVALID_CERTS,
                )?,
            )?,
            mailbox: optional_string(vars, "IMAP_MAILBOX", DEFAULT_IMAP_MAILBOX)?,
            poll_interval: parse_positive_duration(
                "IMAP_POLL_INTERVAL",
                &optional_string(vars, "IMAP_POLL_INTERVAL", DEFAULT_IMAP_POLL_INTERVAL)?,
            )?,
            force_reconnect: parse_positive_duration(
                "IMAP_FORCE_RECONNECT",
                &optional_string(vars, "IMAP_FORCE_RECONNECT", DEFAULT_IMAP_FORCE_RECONNECT)?,
            )?,
            mark_seen: parse_bool(
                "IMAP_MARK_SEEN",
                &optional_string(vars, "IMAP_MARK_SEEN", DEFAULT_IMAP_MARK_SEEN)?,
            )?,
        })
    }
}

impl fmt::Debug for ImapConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("use_tls", &self.use_tls)
            .field("accept_invalid_certs", &self.accept_invalid_certs)
            .field("mailbox", &self.mailbox)
            .field("poll_interval", &self.poll_interval)
            .field("force_reconnect", &self.force_reconnect)
            .field("mark_seen", &self.mark_seen)
            .finish()
    }
}

#[derive(Clone)]
pub struct TelegramConfig {
    pub token: SecretString,
    pub chat_id: i64,
}

impl TelegramConfig {
    fn from_env_map(vars: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        let chat_id = parse_i64(
            "TELEGRAM_CHAT_ID",
            &required_string(vars, "TELEGRAM_CHAT_ID")?,
        )?;
        if chat_id == 0 {
            return Err(invalid_value(
                "TELEGRAM_CHAT_ID",
                "0",
                "a non-zero i64 Telegram chat id",
            ));
        }

        Ok(Self {
            token: SecretString::from(required_string(vars, "TELEGRAM_TOKEN")?),
            chat_id,
        })
    }
}

impl fmt::Debug for TelegramConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramConfig")
            .field("token", &"[REDACTED]")
            .field("chat_id", &self.chat_id)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct HttpConfig {
    pub addr: String,
}

impl HttpConfig {
    fn from_env_map(vars: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        Ok(Self {
            addr: optional_string(vars, "HTTP_ADDR", DEFAULT_HTTP_ADDR)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ViewerConfig {
    pub url_base: Url,
    pub page_ttl: Duration,
    pub page_max_views: Option<u32>,
    pub remote_images: ViewerRemoteImages,
}

impl ViewerConfig {
    fn from_env_map(vars: &BTreeMap<String, String>) -> Result<Self, ConfigError> {
        Ok(Self {
            url_base: parse_viewer_url_base(
                "VIEWER_URL_BASE",
                &required_string(vars, "VIEWER_URL_BASE")?,
            )?,
            page_ttl: parse_positive_duration(
                "VIEWER_PAGE_TTL",
                &optional_string(vars, "VIEWER_PAGE_TTL", DEFAULT_VIEWER_PAGE_TTL)?,
            )?,
            page_max_views: parse_max_views(
                "VIEWER_PAGE_MAX_VIEWS",
                &optional_string(vars, "VIEWER_PAGE_MAX_VIEWS", DEFAULT_VIEWER_PAGE_MAX_VIEWS)?,
            )?,
            remote_images: parse_remote_images(
                "VIEWER_REMOTE_IMAGES",
                &optional_string(vars, "VIEWER_REMOTE_IMAGES", DEFAULT_VIEWER_REMOTE_IMAGES)?,
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerRemoteImages {
    Allow,
    Block,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("required env {key} is not set")]
    MissingRequired { key: &'static str },

    #[error("env {key} has invalid value {value:?}; expected {expected}")]
    InvalidValue {
        key: &'static str,
        value: String,
        expected: &'static str,
    },

    #[error("MAIL_SOURCE={mail_source:?} is not supported: {reason}")]
    UnsupportedMailSource {
        mail_source: String,
        reason: &'static str,
    },
}

fn required_string(
    vars: &BTreeMap<String, String>,
    key: &'static str,
) -> Result<String, ConfigError> {
    match vars
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        Some(value) => Ok(value.to_owned()),
        None => Err(ConfigError::MissingRequired { key }),
    }
}

fn optional_string(
    vars: &BTreeMap<String, String>,
    key: &'static str,
    default: &str,
) -> Result<String, ConfigError> {
    match vars.get(key).map(|value| value.trim()) {
        Some("") => Err(invalid_value(
            key,
            "",
            "a non-empty value or unset to use default",
        )),
        Some(value) => Ok(value.to_owned()),
        None => Ok(default.to_owned()),
    }
}

fn parse_mail_source_kind(key: &'static str, value: &str) -> Result<MailSourceKind, ConfigError> {
    MailSourceKind::parse(value)
        .ok_or_else(|| invalid_value(key, value, "one of: imap, proton_custom (reserved)"))
}

fn parse_bool(key: &'static str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" => Ok(true),
        "0" | "false" | "no" | "n" => Ok(false),
        _ => Err(invalid_value(key, value, "a boolean")),
    }
}

fn parse_i64(key: &'static str, value: &str) -> Result<i64, ConfigError> {
    value
        .trim()
        .parse()
        .map_err(|_| invalid_value(key, value, "an i64 integer"))
}

fn parse_u16(key: &'static str, value: &str) -> Result<u16, ConfigError> {
    value
        .trim()
        .parse()
        .map_err(|_| invalid_value(key, value, "a TCP port in range 0..=65535"))
}

fn parse_max_views(key: &'static str, value: &str) -> Result<Option<u32>, ConfigError> {
    let parsed = value
        .trim()
        .parse::<i64>()
        .map_err(|_| invalid_value(key, value, "an integer; <= 0 means unlimited"))?;

    if parsed <= 0 {
        return Ok(None);
    }

    u32::try_from(parsed)
        .map(Some)
        .map_err(|_| invalid_value(key, value, "an integer in range 1..=4294967295"))
}

fn parse_remote_images(key: &'static str, value: &str) -> Result<ViewerRemoteImages, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "allow" => Ok(ViewerRemoteImages::Allow),
        "block" => Ok(ViewerRemoteImages::Block),
        _ => Err(invalid_value(key, value, "one of: allow, block")),
    }
}

fn parse_viewer_url_base(key: &'static str, value: &str) -> Result<Url, ConfigError> {
    let url = Url::parse(value)
        .map_err(|_| invalid_value(key, value, "an absolute http(s) URL ending with /view"))?;

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(invalid_value(
            key,
            value,
            "an absolute http(s) URL ending with /view",
        ));
    }

    if !url.path().ends_with("/view") {
        return Err(invalid_value(key, value, "URL path ending with /view"));
    }

    Ok(url)
}

fn parse_positive_duration(key: &'static str, value: &str) -> Result<Duration, ConfigError> {
    let duration = parse_go_duration(value)
        .ok_or_else(|| invalid_value(key, value, "a positive duration like 60s, 5m, or 48h"))?;

    if duration.is_zero() {
        return Err(invalid_value(
            key,
            value,
            "a positive duration greater than zero",
        ));
    }

    Ok(duration)
}

fn parse_go_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('-') {
        return None;
    }

    if value.chars().all(|character| character.is_ascii_digit()) {
        let seconds = value.parse::<u64>().ok()?;
        return Some(Duration::from_secs(seconds));
    }

    let mut index = 0;
    let mut total_nanos = 0_u128;

    while index < value.len() {
        let number_start = index;
        while index < value.len() && value.as_bytes()[index].is_ascii_digit() {
            index += 1;
        }

        if number_start == index {
            return None;
        }

        let number = value[number_start..index].parse::<u128>().ok()?;
        let rest = &value[index..];
        let (multiplier, unit_len) = duration_unit(rest)?;
        let addition = number.checked_mul(multiplier)?;
        total_nanos = total_nanos.checked_add(addition)?;
        index += unit_len;
    }

    let seconds = total_nanos / 1_000_000_000;
    let nanos = total_nanos % 1_000_000_000;

    if seconds > u64::MAX.into() {
        return None;
    }

    Some(Duration::new(seconds as u64, nanos as u32))
}

fn duration_unit(value: &str) -> Option<(u128, usize)> {
    if value.starts_with("ns") {
        Some((1, 2))
    } else if value.starts_with("us") {
        Some((1_000, 2))
    } else if value.starts_with("µs") {
        Some((1_000, "µs".len()))
    } else if value.starts_with("ms") {
        Some((1_000_000, 2))
    } else if value.starts_with('s') {
        Some((1_000_000_000, 1))
    } else if value.starts_with('m') {
        Some((60 * 1_000_000_000, 1))
    } else if value.starts_with('h') {
        Some((60 * 60 * 1_000_000_000, 1))
    } else {
        None
    }
}

fn invalid_value(
    key: &'static str,
    value: impl Into<String>,
    expected: &'static str,
) -> ConfigError {
    ConfigError::InvalidValue {
        key,
        value: value.into(),
        expected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_env() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("IMAP_HOST".to_owned(), "imap.example.com".to_owned()),
            ("IMAP_USERNAME".to_owned(), "user@example.com".to_owned()),
            ("IMAP_PASSWORD".to_owned(), "secret".to_owned()),
            ("TELEGRAM_TOKEN".to_owned(), "123:secret".to_owned()),
            ("TELEGRAM_CHAT_ID".to_owned(), "42".to_owned()),
            (
                "VIEWER_URL_BASE".to_owned(),
                "https://mail.example.com/view".to_owned(),
            ),
        ])
    }

    #[test]
    fn loads_defaults() {
        let config = Config::from_env_map(&base_env()).expect("config should load");

        assert_eq!(config.mail_source.kind(), MailSourceKind::Imap);
        assert_eq!(config.telegram.chat_id, 42);
        assert_eq!(config.http.addr, ":8080");
        assert_eq!(config.viewer.page_ttl, Duration::from_secs(48 * 60 * 60));
        assert_eq!(config.viewer.page_max_views, Some(3));
        assert_eq!(config.viewer.remote_images, ViewerRemoteImages::Allow);
        assert_eq!(config.log_filter, "info");

        let MailSourceConfig::Imap(imap) = config.mail_source;
        assert_eq!(imap.host, "imap.example.com");
        assert_eq!(imap.port, 993);
        assert!(imap.use_tls);
        assert!(!imap.accept_invalid_certs);
        assert_eq!(imap.mailbox, "INBOX");
        assert_eq!(imap.poll_interval, Duration::from_secs(60));
        assert_eq!(imap.force_reconnect, Duration::from_secs(60));
        assert!(!imap.mark_seen);
    }

    #[test]
    fn missing_required_env_fails() {
        let mut vars = base_env();
        vars.remove("IMAP_HOST");

        assert!(matches!(
            Config::from_env_map(&vars),
            Err(ConfigError::MissingRequired { key: "IMAP_HOST" })
        ));
    }

    #[test]
    fn invalid_duration_fails() {
        let mut vars = base_env();
        vars.insert("IMAP_POLL_INTERVAL".to_owned(), "soon".to_owned());

        assert!(matches!(
            Config::from_env_map(&vars),
            Err(ConfigError::InvalidValue {
                key: "IMAP_POLL_INTERVAL",
                ..
            })
        ));
    }

    #[test]
    fn empty_optional_env_fails_fast() {
        let mut vars = base_env();
        vars.insert("VIEWER_PAGE_TTL".to_owned(), "  ".to_owned());

        assert!(matches!(
            Config::from_env_map(&vars),
            Err(ConfigError::InvalidValue {
                key: "VIEWER_PAGE_TTL",
                ..
            })
        ));
    }

    #[test]
    fn invalid_chat_id_fails() {
        let mut vars = base_env();
        vars.insert("TELEGRAM_CHAT_ID".to_owned(), "not-a-number".to_owned());

        assert!(matches!(
            Config::from_env_map(&vars),
            Err(ConfigError::InvalidValue {
                key: "TELEGRAM_CHAT_ID",
                ..
            })
        ));
    }

    #[test]
    fn non_positive_max_views_means_unlimited() {
        let mut vars = base_env();
        vars.insert("VIEWER_PAGE_MAX_VIEWS".to_owned(), "0".to_owned());

        let config = Config::from_env_map(&vars).expect("config should load");

        assert_eq!(config.viewer.page_max_views, None);
    }

    #[test]
    fn viewer_url_must_end_with_view() {
        let mut vars = base_env();
        vars.insert(
            "VIEWER_URL_BASE".to_owned(),
            "https://mail.example.com/message".to_owned(),
        );

        assert!(matches!(
            Config::from_env_map(&vars),
            Err(ConfigError::InvalidValue {
                key: "VIEWER_URL_BASE",
                ..
            })
        ));
    }

    #[test]
    fn proton_custom_is_reserved_but_not_implemented() {
        let mut vars = base_env();
        vars.insert("MAIL_SOURCE".to_owned(), "proton_custom".to_owned());

        assert!(matches!(
            Config::from_env_map(&vars),
            Err(ConfigError::UnsupportedMailSource { mail_source, .. }) if mail_source == "proton_custom"
        ));
    }

    #[test]
    fn parses_compound_go_like_duration() {
        assert_eq!(
            parse_go_duration("1h30m5s"),
            Some(Duration::from_secs(5_405))
        );
    }
}
