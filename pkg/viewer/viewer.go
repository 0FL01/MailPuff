package viewer

import (
    "crypto/rand"
    "encoding/base64"
    "errors"
    "strings"
    "log"
    "net/http"
    "sync"
    "time"

    "github.com/google/uuid"
    "github.com/microcosm-cc/bluemonday"
)

// Page представляет опубликованную HTML-страницу с контролем срока жизни и просмотров.
type Page struct {
	ID         string
	Token      string
	HTML       string
	CreatedAt  time.Time
	ExpiresAt  time.Time
	MaxViews   int
	Views      int
	ChatID     int64
	MessageID  int
	IMAPUID    int
}

// OnDeleteCallback вызывается при удалении страницы (по TTL или из-за превышения просмотров).
// reason: "expired" | "max_views" | "manual" | "not_found"
type OnDeleteCallback func(p *Page, reason string)

// Store хранит страницы в памяти и предоставляет HTTP-доступ к ним.
type Store struct {
	mu              sync.RWMutex
	pages           map[string]*Page
	defaultTTL      time.Duration
	defaultMaxViews int
	onDelete        OnDeleteCallback
	onFirstView     func(*Page)
}

// sanitizePolicy — настраиваемая политика очистки HTML, адаптированная под разметку писем.
// Базируемся на UGCPolicy, затем разрешаем распространённые элементы и атрибуты,
// используемые в email‑вёрстке (таблицы, inline‑стили, размеры картинок и т.п.).
// Скрипты и обработчики событий по‑прежнему удаляются bluemonday.
var sanitizePolicy = func() *bluemonday.Policy {
    p := bluemonday.UGCPolicy()
    // Разрешаем таблицы и типичные атрибуты таблиц
    p.AllowElements("table", "thead", "tbody", "tfoot", "tr", "td", "th", "col", "colgroup")
    p.AllowAttrs("align", "valign", "border", "cellpadding", "cellspacing", "bgcolor", "width", "height").OnElements("table")
    p.AllowAttrs("colspan", "rowspan", "align", "valign", "width", "height").OnElements("td", "th")

    // Разрешаем ограниченное использование inline‑стилей на базовых контейнерах
    p.AllowAttrs("style").OnElements("table", "tr", "td", "th", "div", "span", "p", "img", "a", "ul", "ol", "li")

    // Картинки: стандартные атрибуты + data: и cid: источники
    p.AllowAttrs("src", "alt", "title", "width", "height").OnElements("img")
    p.AllowDataURIImages()
    p.AllowURLSchemes("http", "https", "mailto", "tel", "cid")

    // Ссылки: target/rel часто встречаются в письмах
    p.AllowAttrs("target", "rel").OnElements("a")

    // Нестрогие URL по умолчанию уже проверяются bluemonday; relative URL оставляем запрещёнными
    return p
}()

// redactID маскирует чувствительный идентификатор страницы для логов
// оставляя только небольшой фрагмент для корреляции.
func redactID(id string) string {
    if len(id) == 0 {
        return "(empty)"
    }
    if len(id) <= 8 {
        return "…"
    }
    return id[:4] + "…" + id[len(id)-4:]
}

func NewStore(defaultTTL time.Duration, defaultMaxViews int) *Store {
	return &Store{
		pages:           make(map[string]*Page),
		defaultTTL:      defaultTTL,
		defaultMaxViews: defaultMaxViews,
	}
}

func (s *Store) SetOnDelete(cb OnDeleteCallback) {
	s.mu.Lock()
	s.onDelete = cb
	s.mu.Unlock()
}

// SetOnFirstView регистрирует колбэк, вызываемый при самом первом успешном открытии страницы.
func (s *Store) SetOnFirstView(cb func(*Page)) {
	s.mu.Lock()
	s.onFirstView = cb
	s.mu.Unlock()
}

// generateToken генерирует URL-safe токен длиной n байт (до base64url).
func generateToken(n int) (string, error) {
	b := make([]byte, n)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	// Без padding, чтобы URL был компактным
	return base64.RawURLEncoding.EncodeToString(b), nil
}

// CreatePage добавляет страницу в хранилище и планирует удаление по TTL.
// Если ttl == 0, используется defaultTTL. Если maxViews <= 0, просмотры не ограничены.
func (s *Store) CreatePage(html string, ttl time.Duration, maxViews int) (id, token string, err error) {
	if html == "" {
		return "", "", errors.New("empty html")
	}
	if ttl <= 0 {
		ttl = s.defaultTTL
	}
	if maxViews <= 0 {
		maxViews = s.defaultMaxViews
	}
    // Санитизируем HTML перед сохранением, чтобы защититься от XSS в письмах
    sanitized := sanitizePolicy.Sanitize(html)
    if strings.TrimSpace(sanitized) == "" {
        return "", "", errors.New("empty html after sanitization")
    }
	uid := uuid.NewString()
	tok, err := generateToken(18)
	if err != nil {
		return "", "", err
	}
	now := time.Now()
	p := &Page{
		ID:        uid,
		Token:     tok,
        HTML:      sanitized,
		CreatedAt: now,
		ExpiresAt: now.Add(ttl),
		MaxViews:  maxViews,
	}
	// Сохраняем страницу
	s.mu.Lock()
	s.pages[uid] = p
	s.mu.Unlock()

	// Планируем удаление по TTL
	time.AfterFunc(ttl, func() {
		deleted := s.delete(uid, "expired")
		if deleted != nil && s.getOnDelete() != nil {
			go s.getOnDelete()(deleted, "expired")
		}
	})

	return uid, tok, nil
}

// SetIMAPUID привязывает к странице UID письма из IMAP для последующих действий.
func (s *Store) SetIMAPUID(id string, uid int) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	p, ok := s.pages[id]
	if !ok {
		return false
	}
	p.IMAPUID = uid
	return true
}

// SetMessageRef привязывает к странице информацию о Telegram-сообщении для последующего удаления.
func (s *Store) SetMessageRef(id string, chatID int64, messageID int) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	p, ok := s.pages[id]
	if !ok {
		return false
	}
	p.ChatID = chatID
	p.MessageID = messageID
	return true
}

// View возвращает HTML страницы при корректном токене. Увеличивает счётчик просмотров.
// Если лимит просмотров превышен после этого просмотра, страница удаляется и колбэк вызывается.
// ViewWithReason возвращает HTML и детальную причину отказа вместо простого bool.
// Возможные reason: "", "not_found", "invalid_token", "expired".
func (s *Store) ViewWithReason(id, token string) (html string, ok bool, reason string) {
    s.mu.Lock()
    defer s.mu.Unlock()
    p, exists := s.pages[id]
    if !exists {
        return "", false, "not_found"
    }
    if token == "" || token != p.Token {
        return "", false, "invalid_token"
    }
    // Проверка срока годности
    if time.Now().After(p.ExpiresAt) {
        // Удаляем как просроченную
        delete(s.pages, id)
        cb := s.onDelete
        if cb != nil {
            go cb(p, "expired")
        }
        return "", false, "expired"
    }
    // Разрешаем просмотр
    firstView := p.Views == 0
    p.Views++
    content := p.HTML
    // Колбэк самого первого просмотра
    if firstView && s.onFirstView != nil {
        go s.onFirstView(p)
    }
    // Если задан лимит и он превышен — удаляем после этого просмотра
    if p.MaxViews > 0 && p.Views > p.MaxViews {
        delete(s.pages, id)
        cb := s.onDelete
        if cb != nil {
            go cb(p, "max_views")
        }
    }
    return content, true, ""
}

// View сохраняет обратную совместимость: возвращает только html и ok.
func (s *Store) View(id, token string) (html string, ok bool) {
    html, ok, _ = s.ViewWithReason(id, token)
    return html, ok
}

// Delete удаляет страницу вручную и вызывает onDelete.
func (s *Store) Delete(id string) bool {
	deleted := s.delete(id, "manual")
	if deleted != nil && s.getOnDelete() != nil {
		go s.getOnDelete()(deleted, "manual")
		return true
	}
	return deleted != nil
}

// delete — внутренняя версия без вызова колбэка снаружи.
func (s *Store) delete(id, reason string) *Page {
	s.mu.Lock()
	defer s.mu.Unlock()
	p, ok := s.pages[id]
	if !ok {
		return nil
	}
	delete(s.pages, id)
	return p
}

func (s *Store) getOnDelete() OnDeleteCallback {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.onDelete
}

// StartHTTPServer запускает простой HTTP-сервер с эндпоинтом /view?id=UUID&token=TOKEN
func StartHTTPServer(addr string, store *Store) error {
	mux := http.NewServeMux()
	mux.HandleFunc("/view", func(w http.ResponseWriter, r *http.Request) {
		id := r.URL.Query().Get("id")
		tok := r.URL.Query().Get("token")
        if id == "" || tok == "" {
            // Логируем причину, не раскрывая токен
            log.Printf("view 404 reason=missing_params ip=%s id=%s", r.RemoteAddr, redactID(id))
			http.NotFound(w, r)
			return
		}
        html, ok, reason := store.ViewWithReason(id, tok)
        if !ok {
            // Детально логируем причину (token не логируем), id маскируем
            log.Printf("view 404 reason=%s ip=%s id=%s", reason, r.RemoteAddr, redactID(id))
			http.NotFound(w, r)
			return
		}
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		_, _ = w.Write([]byte(html))
	})

    server := &http.Server{Addr: addr, Handler: logRequest(mux)}
	log.Printf("http server listening on %s", addr)
	return server.ListenAndServe()
}

// statusRecorder фиксирует HTTP-статус и размер ответа.
type statusRecorder struct {
    http.ResponseWriter
    status int
    size   int
}

func (sr *statusRecorder) WriteHeader(code int) {
    sr.status = code
    sr.ResponseWriter.WriteHeader(code)
}

func (sr *statusRecorder) Write(b []byte) (int, error) {
    if sr.status == 0 {
        sr.status = http.StatusOK
    }
    n, err := sr.ResponseWriter.Write(b)
    sr.size += n
    return n, err
}

func logRequest(next http.Handler) http.Handler {
    return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
        start := time.Now()
        rec := &statusRecorder{ResponseWriter: w}
        next.ServeHTTP(rec, r)
        dur := time.Since(start)
        log.Printf("%s %s status=%d size=%d duration=%s", r.Method, r.URL.Path, rec.status, rec.size, dur)
    })
}

