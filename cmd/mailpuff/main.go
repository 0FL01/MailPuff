package main

import (
    "crypto/rand"
    "encoding/base64"
    "log"
    "net/url"
    "time"
    "strings"
    "sync"

    tgbotapi "github.com/go-telegram-bot-api/telegram-bot-api/v5"

    "mailpuff/pkg/config"
    "mailpuff/pkg/email"
    imapPkg "mailpuff/pkg/imap"
    "mailpuff/pkg/telegram"
    "mailpuff/pkg/viewer"
)
// answerCallback отправляет ответ на CallbackQuery, чтобы Telegram показал всплывающее уведомление
func answerCallback(bot *tgbotapi.BotAPI, callbackID string, text string) error {
    cfg := tgbotapi.CallbackConfig{
        CallbackQueryID: callbackID,
        Text:            text,
        ShowAlert:       false,
        CacheTime:       0,
    }
    _, err := bot.Request(cfg)
    return err
}

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

// buildMarkCallbackData формирует callback data для кнопки "Mark as read".
// Формат: "mark:<key>" (ключ хранится локально в памяти и маппится на id/token)
func buildMarkCallbackData(key string) string {
    return "mark:" + key
}

// genCallbackKey генерирует короткий URL-safe ключ для callback data
func genCallbackKey(n int) string {
    b := make([]byte, n)
    if _, err := rand.Read(b); err != nil {
        return "fallback"
    }
    return base64.RawURLEncoding.EncodeToString(b)
}

// локальная память для соответствия callback key -> (id, token)
type markCallbackPayload struct {
    ID    string
    Token string
}

var markCbMap sync.Map

// tgMessageRef хранит сведения, необходимые для редактирования клавиатуры сообщения
// при автоматическом скрытии кнопки "Mark as read".
type tgMessageRef struct {
    chatID    int64
    messageID int
    id        string
    token     string
}

// uidToMsg сопоставляет IMAP UID -> ссылку на Telegram-сообщение и страницу viewer
var uidToMsg sync.Map

// pageToCbKey сопоставляет pageID -> callback key, чтобы можно было
// удалить key из локального кэша после скрытия кнопки
var pageToCbKey sync.Map

// hideMarkButton обновляет клавиатуру сообщения, оставляя только кнопку "Open html".
func hideMarkButton(bot *tgbotapi.BotAPI, chatID int64, messageID int, viewerURL string) error {
    btnView := tgbotapi.NewInlineKeyboardButtonURL("Open html", viewerURL)
    newMarkup := tgbotapi.NewInlineKeyboardMarkup(tgbotapi.NewInlineKeyboardRow(btnView))
    edit := tgbotapi.NewEditMessageReplyMarkup(chatID, messageID, newMarkup)
    _, err := bot.Request(edit)
    return err
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

        // После успешной отметки как прочитанного — скрываем кнопку в Telegram-сообщении
        if p.ChatID != 0 && p.MessageID != 0 && p.ID != "" && p.Token != "" {
            viewerURL := buildViewerURL(cfg.ViewerBaseURL, p.ID, p.Token)
            if err := hideMarkButton(bot, p.ChatID, p.MessageID, viewerURL); err != nil {
                log.Printf("tg edit keyboard on first-view uid=%d chat_id=%d msg_id=%d err=%v", p.IMAPUID, p.ChatID, p.MessageID, err)
            }
        }
        // Чистим callback key и карту UID -> сообщение
        if p.ID != "" {
            if v, ok := pageToCbKey.Load(p.ID); ok {
                if key, _ := v.(string); key != "" {
                    markCbMap.Delete(key)
                }
                pageToCbKey.Delete(p.ID)
            }
        }
        if p.IMAPUID > 0 {
            uidToMsg.Delete(p.IMAPUID)
        }
	})
    // Обработчик для пометки прочитанным через IMAP (переиспользуется HTTP и Telegram callback)
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

    // HTTP сервер: поддержка /view и /mark_read
    go func() {
        if err := viewer.StartHTTPServer(cfg.HTTPAddr, store, markSeen); err != nil {
            log.Fatalf("http server error: %v", err)
        }
    }()

    // Telegram updates: обработка нажатий на кнопку Mark as read (callback)
    go func() {
        u := tgbotapi.NewUpdate(0)
        u.Timeout = 60
        updates := bot.GetUpdatesChan(u)
        for upd := range updates {
            if upd.CallbackQuery == nil {
                continue
            }
            data := upd.CallbackQuery.Data
            if !strings.HasPrefix(data, "mark:") {
                continue
            }

            // Ожидаем формат: mark:<key>
            parts := strings.SplitN(data, ":", 2)
            if len(parts) != 2 {
                _ = answerCallback(bot, upd.CallbackQuery.ID, "Invalid data")
                log.Printf("tg callback invalid_data chat_id=%d msg_id=%d data=%q", upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, data)
                continue
            }
            key := parts[1]
            payloadV, ok := markCbMap.Load(key)
            if !ok {
                _ = answerCallback(bot, upd.CallbackQuery.ID, "Link expired")
                log.Printf("tg callback mark_read 404 reason=cbkey_not_found chat_id=%d msg_id=%d key=%q", upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, key)
                continue
            }
            payload := payloadV.(markCallbackPayload)
            id := payload.ID
            tok := payload.Token

            page, ok, reason := store.Authorize(id, tok)
            if !ok {
                _ = answerCallback(bot, upd.CallbackQuery.ID, "Link expired or invalid")
                log.Printf("tg callback mark_read 404 reason=%s chat_id=%d msg_id=%d id=%s", reason, upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, maskID(id))
                continue
            }
            if page.IMAPUID <= 0 {
                _ = answerCallback(bot, upd.CallbackQuery.ID, "IMAP UID missing")
                log.Printf("tg callback mark_read 404 reason=missing_imap_uid chat_id=%d msg_id=%d id=%s", upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, maskID(id))
                continue
            }
            if err := markSeen(page.IMAPUID); err != nil {
                _ = answerCallback(bot, upd.CallbackQuery.ID, "Failed to mark as read")
                log.Printf("tg callback mark_read 500 uid=%d id=%s err=%v", page.IMAPUID, maskID(id), err)
                continue
            }

            // Успех: отвечаем всплывашкой и обновляем клавиатуру (убираем Mark as read)
            _ = answerCallback(bot, upd.CallbackQuery.ID, "Marked as read")
            markCbMap.Delete(key)

            viewerURL := buildViewerURL(cfg.ViewerBaseURL, id, tok)
            if err := hideMarkButton(bot, upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, viewerURL); err != nil {
                log.Printf("tg callback mark_read edit_keyboard error chat_id=%d msg_id=%d err=%v", upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, err)
            }

            // Очистка привязок для предотвращения повторной обработки
            uidToMsg.Delete(page.IMAPUID)
            pageToCbKey.Delete(id)

            log.Printf("tg callback mark_read ok uid=%d chat_id=%d msg_id=%d id=%s", page.IMAPUID, upd.CallbackQuery.Message.Chat.ID, upd.CallbackQuery.Message.MessageID, maskID(id))
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
                cbKey := genCallbackKey(6)
                markCbMap.Store(cbKey, markCallbackPayload{ID: id, Token: token})
                markCB := buildMarkCallbackData(cbKey)
                msgID, err := telegram.SendMessage(bot, cfg.TelegramChatID, sum.Subject, sum.FromName, sum.FromAddress, viewerURL, markCB)
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