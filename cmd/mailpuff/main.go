package main

import (
    "log"
    "net/url"
    "time"

    tgbotapi "github.com/go-telegram-bot-api/telegram-bot-api/v5"

    "mailpuff/pkg/config"
    "mailpuff/pkg/email"
    imapPkg "mailpuff/pkg/imap"
    "mailpuff/pkg/telegram"
    "mailpuff/pkg/viewer"
)

func buildViewerURL(base, id, token string) string {
	u, err := url.Parse(base)
	if err != nil {
		return base
	}
	q := u.Query()
    q.Set("id", id)
    q.Set("token", token)
	u.RawQuery = q.Encode()
	return u.String()
}

// buildMarkURL формирует URL для действия mark_read на том же хосте, что и viewer base URL.
func buildMarkURL(base, id, token string) string {
    u, err := url.Parse(base)
    if err != nil {
        return base
    }
    // Заменяем путь на /mark_read, сохраняя схему/хост/порт
    u.Path = "/mark_read"
    q := u.Query()
    q.Set("id", id)
    q.Set("token", token)
    u.RawQuery = q.Encode()
    return u.String()
}

func main() {
	cfg := config.Load()
    log.Printf("starting mailpuff poll=%s mailbox=%s http=%s ttl=%s maxViews=%d", cfg.PollInterval, cfg.Mailbox, cfg.HTTPAddr, cfg.ViewerPageTTL, cfg.ViewerPageMaxViews)

	bot, err := tgbotapi.NewBotAPI(cfg.TelegramToken)
	if err != nil {
		log.Fatalf("telegram init error: %v", err)
	}
	// Инициализируем in-memory viewer store и http-сервер
    store := viewer.NewStore(cfg.ViewerPageTTL, cfg.ViewerPageMaxViews)
    store.SetOnDelete(func(p *viewer.Page, reason string) {
        // При удалении страницы оставляем Telegram-сообщение; удаляем только страницу из памяти
        if p != nil {
            // Маскируем id
            masked := maskID(p.ID)
            log.Printf("cleanup: page id=%s reason=%s kept_telegram_message chat_id=%d msg_id=%d", masked, reason, p.ChatID, p.MessageID)
        }
    })
	// При первом открытии страницы — опционально помечаем письмо прочитанным в IMAP
	store.SetOnFirstView(func(p *viewer.Page) {
		if p == nil {
			return
		}
		if !cfg.MarkSeen {
			return
		}
		if p.IMAPUID <= 0 {
			return
		}
		imapCfg := imapPkg.Config{
			Host:     cfg.IMAPHost,
			Port:     cfg.IMAPPort,
			Username: cfg.IMAPUsername,
			Password: cfg.IMAPPassword,
			UseTLS:   cfg.IMAPUseTLS,
			Mailbox:  cfg.Mailbox,
		}
		m, err := imapPkg.ConnectAndSelect(imapCfg)
        if err != nil {
            log.Printf("imap connect error during first-view mark-seen: %v", err)
			return
		}
		defer func() { _ = m.Close() }()
        if err := imapPkg.MarkSeen(m, p.IMAPUID); err != nil {
            log.Printf("imap mark_seen error uid=%d: %v", p.IMAPUID, err)
			return
		}
        log.Printf("imap mark_seen ok uid=%d on first HTML view", p.IMAPUID)
	})
    go func() {
        // Обработчик для /mark_read: подключаемся к IMAP и помечаем письмо прочитанным
        markSeen := func(uid int) error {
            imapCfg := imapPkg.Config{
                Host:     cfg.IMAPHost,
                Port:     cfg.IMAPPort,
                Username: cfg.IMAPUsername,
                Password: cfg.IMAPPassword,
                UseTLS:   cfg.IMAPUseTLS,
                Mailbox:  cfg.Mailbox,
            }
            m, err := imapPkg.ConnectAndSelect(imapCfg)
            if err != nil {
                return err
            }
            defer func() { _ = m.Close() }()
            return imapPkg.MarkSeen(m, uid)
        }
        if err := viewer.StartHTTPServer(cfg.HTTPAddr, store, markSeen); err != nil {
            log.Fatalf("http server error: %v", err)
        }
    }()

    processed := make(map[int]struct{})

	for {
		imapCfg := imapPkg.Config{
			Host:     cfg.IMAPHost,
			Port:     cfg.IMAPPort,
			Username: cfg.IMAPUsername,
			Password: cfg.IMAPPassword,
			UseTLS:   cfg.IMAPUseTLS,
			Mailbox:  cfg.Mailbox,
		}
        c, err := imapPkg.ConnectAndSelect(imapCfg)
        if err != nil {
            log.Printf("imap connect error host=%s port=%d mailbox=%s: %v", imapCfg.Host, imapCfg.Port, imapCfg.Mailbox, err)
			time.Sleep(cfg.PollInterval)
			continue
		}
		func() {
			defer func() {
                _ = c.Close()
			}()
            uids, err := imapPkg.SearchUnseen(c)
            if err != nil {
                log.Printf("imap search_unseen error: %v", err)
				return
			}
            emailsMap, err := imapPkg.FetchEmails(c, uids)
            if err != nil {
                log.Printf("imap fetch_emails error uids=%v: %v", uids, err)
				return
			}
            for uid, em := range emailsMap {
                if uid == 0 {
                    continue
                }
                if _, seen := processed[uid]; seen {
                    continue
                }
                sum := email.Summarize(em)
                if sum.HTMLBody == "" {
                    log.Printf("email skip uid=%d reason=no_body", uid)
                    processed[uid] = struct{}{}
					continue
				}
                // Создаём страницу в хранилище
                id, token, err := store.CreatePage(sum.HTMLBody, cfg.ViewerPageTTL, cfg.ViewerPageMaxViews)
                if err != nil {
                    log.Printf("viewer create_page error uid=%d: %v", uid, err)
                    processed[uid] = struct{}{}
                    continue
                }
                viewerURL := buildViewerURL(cfg.ViewerBaseURL, id, token)
                markURL := buildMarkURL(cfg.ViewerBaseURL, id, token)
                msgID, err := telegram.SendMessage(bot, cfg.TelegramChatID, sum.Subject, sum.FromName, sum.FromAddress, viewerURL, markURL)
                if err != nil {
                    log.Printf("telegram send error uid=%d: %v", uid, err)
                    processed[uid] = struct{}{}
					continue
				}
                store.SetMessageRef(id, cfg.TelegramChatID, msgID)
				_ = store.SetIMAPUID(id, uid)
                log.Printf("sent telegram message msg_id=%d uid=%d page_id=%s", msgID, uid, maskID(id))
                processed[uid] = struct{}{}

			}
		}()
		time.Sleep(cfg.PollInterval)
	}
}

// maskID скрывает чувствительные идентификаторы (UUID) в логах, оставляя только часть.
func maskID(id string) string {
    if len(id) == 0 {
        return "(empty)"
    }
    if len(id) <= 8 {
        return "…"
    }
    return id[:4] + "…" + id[len(id)-4:]
}