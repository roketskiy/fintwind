package backendremote

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/gorilla/websocket"
)

type Handler func(context.Context, json.RawMessage) (any, error)

type Tunnel struct {
	Origin    string
	Route     [32]byte
	Secret    [32]byte
	Token     string
	Handle    Handler
	Status    func(string)
	mu        sync.Mutex
	mutations map[string]struct{}
}

type relayFrame struct {
	Type string `json:"t"`
	ID   string `json:"id"`
	Path string `json:"path"`
	Text string `json:"s"`
}

type phoneState struct {
	id      string
	send    *Direction
	receive *Direction
	timer   *time.Timer
}

func (tunnel *Tunnel) Run(ctx context.Context) error {
	endpoint, err := url.Parse(tunnel.Origin)
	if err != nil || endpoint.User != nil || endpoint.RawQuery != "" || endpoint.Fragment != "" || (endpoint.Path != "" && endpoint.Path != "/") {
		return errors.New("invalid relay address")
	}
	if tunnel.Origin == RelayOrigin {
		endpoint.Scheme = "wss"
	} else {
		ip := net.ParseIP(endpoint.Hostname())
		if endpoint.Scheme != "http" || ip == nil || !ip.IsLoopback() {
			return errors.New("invalid relay address")
		}
		endpoint.Scheme = "ws"
	}
	if tunnel.Handle == nil || tunnel.Token == "" {
		return errors.New("remote tunnel is not configured")
	}
	route := base64.RawURLEncoding.EncodeToString(tunnel.Route[:])
	hash := sha256.Sum256([]byte(route))
	endpoint.Path = "/agent"
	endpoint.RawQuery = url.Values{"th": {hex.EncodeToString(hash[:])}}.Encode()
	dialer := websocket.Dialer{HandshakeTimeout: 10 * time.Second, Proxy: nil}
	socket, _, err := dialer.DialContext(ctx, endpoint.String(), http.Header{})
	if err != nil {
		return errors.New("relay connection failed")
	}
	defer socket.Close()
	stop := context.AfterFunc(ctx, func() { _ = socket.Close() })
	defer stop()
	socket.SetReadLimit(frameLimit)
	if err := socket.WriteJSON(map[string]any{"t": "register", "tokenHash": hex.EncodeToString(hash[:]), "name": "Fintwind", "proto": 1, "key": tunnel.Token}); err != nil {
		return errors.New("relay registration failed")
	}
	_ = socket.SetReadDeadline(time.Now().Add(10 * time.Second))
	var registered struct {
		Type string `json:"t"`
	}
	if err := socket.ReadJSON(&registered); err != nil || registered.Type != "registered" {
		return errors.New("relay registration failed")
	}
	_ = socket.SetReadDeadline(time.Time{})
	tunnel.status("waiting")
	var phone *phoneState
	defer func() {
		if phone != nil && phone.timer != nil {
			phone.timer.Stop()
		}
	}()
	for {
		_, raw, err := socket.ReadMessage()
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return errors.New("relay connection ended")
		}
		var frame relayFrame
		if json.Unmarshal(raw, &frame) != nil || len(frame.ID) > 255 {
			return errors.New("invalid relay frame")
		}
		switch frame.Type {
		case "hreq":
			if frame.ID == "" {
				return errors.New("invalid relay request")
			}
			if err := socket.WriteJSON(map[string]any{"t": "hres", "id": frame.ID, "status": 403, "headers": map[string]string{"content-length": "0"}}); err != nil {
				return err
			}
			if err := socket.WriteJSON(map[string]any{"t": "hend", "id": frame.ID}); err != nil {
				return err
			}
		case "wopen":
			if frame.ID == "" {
				return errors.New("invalid relay request")
			}
			if frame.Path != "/__fintwind_remote__" || phone != nil {
				if err := socket.WriteJSON(map[string]any{"t": "werr", "id": frame.ID, "message": "Socket unavailable"}); err != nil {
					return err
				}
				continue
			}
			phone = &phoneState{id: frame.ID, timer: time.AfterFunc(8*time.Second, func() { _ = socket.Close() })}
			if err := socket.WriteJSON(map[string]any{"t": "wopened", "id": frame.ID}); err != nil {
				return err
			}
		case "wclose":
			if phone != nil && frame.ID == phone.id {
				phone.timer.Stop()
				phone = nil
				tunnel.status("waiting")
			}
		case "wmsg":
			if phone == nil || frame.ID != phone.id {
				return errors.New("invalid relay socket")
			}
			if err := tunnel.phoneMessage(ctx, socket, phone, frame.Text); err != nil {
				phone.timer.Stop()
				if err := socket.WriteJSON(map[string]any{"t": "wclose", "id": phone.id, "code": 1008}); err != nil {
					return err
				}
				phone = nil
				tunnel.status("waiting")
			}
		default:
			return errors.New("unexpected relay frame")
		}
	}
}

func (tunnel *Tunnel) status(value string) {
	if tunnel.Status != nil {
		tunnel.Status(value)
	}
}

func (tunnel *Tunnel) phoneMessage(ctx context.Context, socket *websocket.Conn, phone *phoneState, text string) error {
	if phone.receive == nil {
		var hello struct {
			Type    string `json:"type"`
			Version int    `json:"version"`
			Nonce   string `json:"nonce"`
		}
		if json.Unmarshal([]byte(text), &hello) != nil || hello.Type != "hello" || hello.Version != 1 {
			return errors.New("invalid hello")
		}
		nonce, err := base64.RawURLEncoding.DecodeString(hello.Nonce)
		if err != nil || len(nonce) != 32 || base64.RawURLEncoding.EncodeToString(nonce) != hello.Nonce {
			return errors.New("invalid nonce")
		}
		var phoneNonce, desktopNonce [32]byte
		copy(phoneNonce[:], nonce)
		if _, err := rand.Read(desktopNonce[:]); err != nil {
			return err
		}
		phone.send, phone.receive, err = Directions(tunnel.Secret, phoneNonce, desktopNonce)
		if err != nil {
			return err
		}
		response, _ := json.Marshal(map[string]any{"type": "hello", "version": 1, "nonce": base64.RawURLEncoding.EncodeToString(desktopNonce[:])})
		if err := socket.WriteJSON(map[string]any{"t": "wmsg", "id": phone.id, "s": string(response)}); err != nil {
			return err
		}
		return sendPhone(socket, phone, map[string]any{"type": "ready"})
	}
	plain, err := phone.receive.Open([]byte(text))
	if err != nil {
		return err
	}
	phone.timer.Stop()
	var request struct {
		ID      string          `json:"id"`
		Command json.RawMessage `json:"command"`
	}
	if validateJSON(plain) != nil || json.Unmarshal(plain, &request) != nil {
		return errors.New("invalid request")
	}
	requestID, parseErr := uuid.Parse(request.ID)
	if parseErr != nil || requestID == uuid.Nil || request.ID != requestID.String() {
		return errors.New("invalid request")
	}
	var command struct {
		Type string `json:"type"`
	}
	if json.Unmarshal(request.Command, &command) != nil || command.Type == "" {
		return errors.New("invalid request")
	}
	tunnel.status("connected")
	readOnly := command.Type == "listWorkspaces" || command.Type == "listSessions" || command.Type == "getSession" || command.Type == "getComposer" || command.Type == "getTaskActivity"
	if !readOnly {
		tunnel.mu.Lock()
		if tunnel.mutations == nil {
			tunnel.mutations = make(map[string]struct{})
		}
		_, exists := tunnel.mutations[request.ID]
		full := len(tunnel.mutations) >= 65536
		if !exists && !full {
			tunnel.mutations[request.ID] = struct{}{}
		}
		tunnel.mu.Unlock()
		if exists || full {
			return sendPhone(socket, phone, map[string]any{"id": request.ID, "ok": false, "error": "Request was already handled or the activation is full"})
		}
	}
	requestCtx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	response, err := tunnel.Handle(requestCtx, request.Command)
	if err != nil {
		return sendPhone(socket, phone, map[string]any{"id": request.ID, "ok": false, "error": "Request failed"})
	}
	return sendPhone(socket, phone, map[string]any{"id": request.ID, "ok": true, "data": response})
}

func sendPhone(socket *websocket.Conn, phone *phoneState, value any) error {
	plain, err := json.Marshal(value)
	if err != nil || len(plain) > frameLimit/2 {
		return errors.New("remote response too large")
	}
	sealed, err := phone.send.Seal(plain)
	if err != nil {
		return err
	}
	return socket.WriteJSON(map[string]any{"t": "wmsg", "id": phone.id, "s": string(sealed)})
}
