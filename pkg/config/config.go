package config

import (
	"log"
	"os"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	IMAPHost       string
	IMAPPort       int
	IMAPUsername   string
	IMAPPassword   string
	IMAPUseTLS     bool
	Mailbox        string
	PollInterval   time.Duration
	ForceReconnect time.Duration
	TelegramToken  string
	TelegramChatID int64
	ViewerBaseURL  string
	MarkSeen       bool
	HTTPAddr       string
	ViewerPageTTL  time.Duration
	ViewerPageMaxViews int
}

func getenv(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func mustGetenv(key string) string {
	v := os.Getenv(key)
	if v == "" {
		log.Fatalf("required env %s is not set", key)
	}
	return v
}

func parseBoolEnv(key string, def bool) bool {
	v := strings.ToLower(strings.TrimSpace(os.Getenv(key)))
	if v == "" {
		return def
	}
	if v == "1" || v == "true" || v == "yes" || v == "y" {
		return true
	}
	if v == "0" || v == "false" || v == "no" || v == "n" {
		return false
	}
	return def
}

func parseIntEnv(key string, def int) int {
	if s := strings.TrimSpace(os.Getenv(key)); s != "" {
		if n, err := strconv.Atoi(s); err == nil {
			return n
		}
	}
	return def
}

func parseInt64Env(key string, def int64) int64 {
	if s := strings.TrimSpace(os.Getenv(key)); s != "" {
		if n, err := strconv.ParseInt(s, 10, 64); err == nil {
			return n
		}
	}
	return def
}

func parseDurationEnv(key string, def time.Duration) time.Duration {
	if s := strings.TrimSpace(os.Getenv(key)); s != "" {
		if d, err := time.ParseDuration(s); err == nil {
			return d
		}
		// allow seconds as integer
		if n, err := strconv.Atoi(s); err == nil {
			return time.Duration(n) * time.Second
		}
	}
	return def
}

func Load() Config {
	cfg := Config{
		IMAPHost:       mustGetenv("IMAP_HOST"),
		IMAPPort:       parseIntEnv("IMAP_PORT", 993),
		IMAPUsername:   mustGetenv("IMAP_USERNAME"),
		IMAPPassword:   mustGetenv("IMAP_PASSWORD"),
		IMAPUseTLS:     parseBoolEnv("IMAP_TLS", true),
		Mailbox:        getenv("IMAP_MAILBOX", "INBOX"),
		PollInterval:   parseDurationEnv("IMAP_POLL_INTERVAL", 60*time.Second),
		ForceReconnect: parseDurationEnv("IMAP_FORCE_RECONNECT", 60*time.Second),
		TelegramToken:  mustGetenv("TELEGRAM_TOKEN"),
		TelegramChatID: parseInt64Env("TELEGRAM_CHAT_ID", 0),
		ViewerBaseURL:  mustGetenv("VIEWER_URL_BASE"),
		MarkSeen:       parseBoolEnv("IMAP_MARK_SEEN", false),
        HTTPAddr:       getenv("HTTP_ADDR", ":8080"),
        ViewerPageTTL:  parseDurationEnv("VIEWER_PAGE_TTL", 48*time.Hour),
        ViewerPageMaxViews: parseIntEnv("VIEWER_PAGE_MAX_VIEWS", 3),
	}
	if cfg.TelegramChatID == 0 {
		log.Fatalf("TELEGRAM_CHAT_ID must be a valid int64")
	}
	return cfg
}