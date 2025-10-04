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

func main() {
	cfg := config.Load()
	log.Printf("starting mailpuff with poll interval %s, mailbox %s", cfg.PollInterval, cfg.Mailbox)

	bot, err := tgbotapi.NewBotAPI(cfg.TelegramToken)
	if err != nil {
		log.Fatalf("telegram init error: %v", err)
	}
	// Инициализируем in-memory viewer store и http-сервер
    store := viewer.NewStore(cfg.ViewerPageTTL, cfg.ViewerPageMaxViews)
    store.SetOnDelete(func(p *viewer.Page, reason string) {
        // При удалении страницы оставляем Telegram-сообщение; удаляем только страницу из памяти
        if p != nil {
            log.Printf("cleanup: page %s removed from memory (%s); telegram message kept", p.ID, reason)
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
			log.Printf("imap connect (first-view mark seen) error: %v", err)
			return
		}
		defer func() { _ = m.Close() }()
		if err := imapPkg.MarkSeen(m, p.IMAPUID); err != nil {
			log.Printf("mark seen UID %d error: %v", p.IMAPUID, err)
			return
		}
		log.Printf("marked UID %d as seen on first HTML view", p.IMAPUID)
	})
    go func() {
        if err := viewer.StartHTTPServer(cfg.HTTPAddr, store); err != nil {
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
			log.Printf("imap connect error: %v", err)
			time.Sleep(cfg.PollInterval)
			continue
		}
		func() {
			defer func() {
                _ = c.Close()
			}()
            uids, err := imapPkg.SearchUnseen(c)
			if err != nil {
				log.Printf("imap search error: %v", err)
				return
			}
            emailsMap, err := imapPkg.FetchEmails(c, uids)
			if err != nil {
				log.Printf("imap fetch error: %v", err)
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
				if err != nil {
                    // Summarize не возвращает ошибку; оставлено на случай будущих изменений
				}
				if sum.HTMLBody == "" {
                    log.Printf("message %d has no HTML or text body", uid)
                    processed[uid] = struct{}{}
					continue
				}
                // Создаём страницу в хранилище
                id, token, err := store.CreatePage(sum.HTMLBody, cfg.ViewerPageTTL, cfg.ViewerPageMaxViews)
                if err != nil {
                    log.Printf("create page error: %v", err)
                    processed[uid] = struct{}{}
                    continue
                }
                viewerURL := buildViewerURL(cfg.ViewerBaseURL, id, token)
                msgID, err := telegram.SendMessage(bot, cfg.TelegramChatID, sum.Subject, sum.FromName, sum.FromAddress, viewerURL)
				if err != nil {
					log.Printf("telegram send error: %v", err)
                    processed[uid] = struct{}{}
					continue
				}
                store.SetMessageRef(id, cfg.TelegramChatID, msgID)
				_ = store.SetIMAPUID(id, uid)
                log.Printf("sent telegram message %d for UID %d; page %s", msgID, uid, id)
                processed[uid] = struct{}{}

			}
		}()
		time.Sleep(cfg.PollInterval)
	}
}