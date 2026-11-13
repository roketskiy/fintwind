package main

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"time"

	"fintwind-go/internal/backendremote"

	"github.com/gorilla/websocket"
)

func run() error {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return err
	}
	defer listener.Close()
	var secret [32]byte
	if _, err := rand.Read(secret[:]); err != nil {
		return err
	}
	result := make(chan error, 1)
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second}
	server.Handler = http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		upgrader := websocket.Upgrader{CheckOrigin: func(*http.Request) bool { return true }}
		socket, err := upgrader.Upgrade(writer, request, nil)
		if err != nil {
			result <- err
			return
		}
		defer socket.Close()
		result <- exchange(socket, secret)
	})
	go server.Serve(listener)
	defer server.Close()
	if err := json.NewEncoder(os.Stdout).Encode(map[string]string{"address": listener.Addr().String(), "secret": base64.RawURLEncoding.EncodeToString(secret[:])}); err != nil {
		return err
	}
	select {
	case err := <-result:
		return err
	case <-time.After(15 * time.Second):
		return errors.New("loopback timed out")
	}
}

func exchange(socket *websocket.Conn, secret [32]byte) error {
	_ = socket.SetReadDeadline(time.Now().Add(10 * time.Second))
	var hello struct {
		Type    string `json:"type"`
		Version int    `json:"version"`
		Nonce   string `json:"nonce"`
	}
	if err := socket.ReadJSON(&hello); err != nil || hello.Type != "hello" || hello.Version != 1 {
		return errors.New("invalid hello")
	}
	phone, err := base64.RawURLEncoding.DecodeString(hello.Nonce)
	if err != nil || len(phone) != 32 {
		return errors.New("invalid nonce")
	}
	var phoneNonce, desktopNonce [32]byte
	copy(phoneNonce[:], phone)
	if _, err := rand.Read(desktopNonce[:]); err != nil {
		return err
	}
	if err := socket.WriteJSON(map[string]any{"type": "hello", "version": 1, "nonce": base64.RawURLEncoding.EncodeToString(desktopNonce[:])}); err != nil {
		return err
	}
	send, receive, err := backendremote.Directions(secret, phoneNonce, desktopNonce)
	if err != nil {
		return err
	}
	ready, err := send.Seal([]byte(`{"type":"ready"}`))
	if err != nil {
		return err
	}
	if err := socket.WriteMessage(websocket.TextMessage, ready); err != nil {
		return err
	}
	_, frame, err := socket.ReadMessage()
	if err != nil {
		return err
	}
	plain, err := receive.Open(frame)
	if err != nil || string(plain) != `{"type":"ping"}` {
		return errors.New("invalid encrypted request")
	}
	answer, err := send.Seal([]byte(`{"type":"pong"}`))
	if err != nil {
		return err
	}
	return socket.WriteMessage(websocket.TextMessage, answer)
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
