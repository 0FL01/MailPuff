package imap

import (
	imap "github.com/BrianLeishman/go-imap"
)

type Config struct {
	Host     string
	Port     int
	Username string
	Password string
	UseTLS   bool
	Mailbox  string
}

// ConnectAndSelect устанавливает соединение и выбирает папку (по умолчанию INBOX)
func ConnectAndSelect(cfg Config) (*imap.Dialer, error) {
	// Библиотека всегда использует TLS; валидация сертификата управляется глобально TLSSkipVerify.
	imap.TLSSkipVerify = false
	if !cfg.UseTLS {
		// Если ранее пользователь явно отключал TLS, ближайшая семантика — не проверять сертификат.
		// Полностью нешифрованные подключения библиотекой не поддерживаются.
		imap.TLSSkipVerify = true
	}

	m, err := imap.New(cfg.Username, cfg.Password, cfg.Host, cfg.Port)
	if err != nil {
		return nil, err
	}
	// Выбираем папку для работы
	mailbox := cfg.Mailbox
	if mailbox == "" {
		mailbox = "INBOX"
	}
	if err := m.SelectFolder(mailbox); err != nil {
		_ = m.Close()
		return nil, err
	}
	return m, nil
}

// SearchUnseen возвращает UIDs непрочитанных писем
func SearchUnseen(m *imap.Dialer) ([]int, error) {
	return m.GetUIDs("UNSEEN")
}

// FetchEmails загружает полные письма по UID'ам
func FetchEmails(m *imap.Dialer, uids []int) (map[int]*imap.Email, error) {
	if len(uids) == 0 {
		return map[int]*imap.Email{}, nil
	}
	return m.GetEmails(uids...)
}

// MarkSeen помечает письмо прочитанным
func MarkSeen(m *imap.Dialer, uid int) error {
	return m.MarkSeen(uid)
}