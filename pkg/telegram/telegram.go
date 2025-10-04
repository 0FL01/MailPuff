package telegram

import (
    "fmt"
    "html"

    telegram "github.com/go-telegram-bot-api/telegram-bot-api/v5"
)

func SendMessage(bot *telegram.BotAPI, chatID int64, subject, fromName, fromAddress, buttonURL string) (int, error) {
    if fromName == "" {
        fromName = "Unknown sender"
    }
    if fromAddress == "" {
        fromAddress = "unknown@unknown"
    }
    // Формирование текста сообщения на основе реальных полей письма
    subjectEsc := html.EscapeString(subject)
    fromNameEsc := html.EscapeString(fromName)
    fromAddressEsc := html.EscapeString(fromAddress)
    text := fmt.Sprintf("%s\n%s\n\nA new email has arrived from this address: %s\n\n🌐 A secret HTML page has been created for it, where you can preview the message by following the link below 👇", subjectEsc, fromNameEsc, fromAddressEsc)
    btn := telegram.NewInlineKeyboardButtonURL("Open secure preview", buttonURL)
	markup := telegram.NewInlineKeyboardMarkup(telegram.NewInlineKeyboardRow(btn))
	msg := telegram.NewMessage(chatID, text)
	msg.ReplyMarkup = markup
	msg.DisableWebPagePreview = true
	msg.ParseMode = "HTML"
	sent, err := bot.Send(msg)
	if err != nil {
		return 0, err
	}
	return sent.MessageID, nil
}

func DeleteMessage(bot *telegram.BotAPI, chatID int64, messageID int) error {
	cfg := telegram.DeleteMessageConfig{ChatID: chatID, MessageID: messageID}
	_, err := bot.Request(cfg)
	return err
}