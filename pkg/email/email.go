package email

import (
	"fmt"
	"html"
	"strings"
	"time"

	bimap "github.com/BrianLeishman/go-imap"
)

type Summary struct {
	Subject     string
	FromName    string
	FromAddress string
	ToAddress   string
	Date        time.Time
	HTMLBody    string
}

// Summarize constructs Summary из структуры письма библиотеки BrianLeishman.
// Берём HTML, иначе разворачиваем text/plain в безопасный <pre>.
func Summarize(e *bimap.Email) Summary {
    var sum Summary
    sum.Subject = e.Subject
    sum.Date = e.Sent
    // Разобрать From и To в адрес/имя по простым паттернам
    parseAddr := func(s string) (addr, name string) {
        s = strings.TrimSpace(s)
        if s == "" {
            return "", ""
        }
        if i := strings.Index(s, "<"); i >= 0 && strings.Contains(s, ">") {
            name = strings.TrimSpace(strings.TrimSpace(s[:i]))
            j := strings.Index(s[i:], ">")
            if j >= 0 {
                addr = strings.TrimSpace(s[i+1 : i+j])
            }
            return addr, name
        }
        if i := strings.Index(s, ":"); i > 0 {
            // format: addr:name
            addr = strings.TrimSpace(s[:i])
            name = strings.TrimSpace(s[i+1:])
            return addr, name
        }
        return s, ""
    }
    // Поля адресов в библиотеке — тип EmailAddresses с методом String()
    sum.FromAddress, sum.FromName = parseAddr(e.From.String())
    toAddr, _ := parseAddr(e.To.String())
    sum.ToAddress = toAddr

    htmlBody := e.HTML
    if htmlBody == "" && e.Text != "" {
        htmlBody = "<pre style=\"white-space:pre-wrap;word-wrap:break-word;\">" + html.EscapeString(e.Text) + "</pre>"
    }
    sum.HTMLBody = htmlBody
    return sum
}

func FormatGistDescription(sum Summary) string {
	date := sum.Date.Format(time.RFC3339)
	from := sum.FromAddress
	to := sum.ToAddress
	if to == "" {
		to = "unknown"
	}
	return fmt.Sprintf("%s - from %s - to %s", date, from, to)
}