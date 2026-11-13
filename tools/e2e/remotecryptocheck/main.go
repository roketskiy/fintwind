package main

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"os"

	"fintwind-go/internal/backendremote"
)

func run() error {
	var input struct {
		Secret, PhoneNonce, DesktopNonce string
		Frame                            json.RawMessage
	}
	if err := json.NewDecoder(io.LimitReader(os.Stdin, 1<<20)).Decode(&input); err != nil {
		return err
	}
	decode := func(text string) ([32]byte, error) {
		var result [32]byte
		value, err := base64.RawURLEncoding.DecodeString(text)
		if err != nil || len(value) != len(result) {
			return result, fmt.Errorf("invalid fixture")
		}
		copy(result[:], value)
		return result, nil
	}
	secret, err := decode(input.Secret)
	if err != nil {
		return err
	}
	phone, err := decode(input.PhoneNonce)
	if err != nil {
		return err
	}
	desktop, err := decode(input.DesktopNonce)
	if err != nil {
		return err
	}
	send, receive, err := backendremote.Directions(secret, phone, desktop)
	if err != nil {
		return err
	}
	duplicate := bytes.Replace(input.Frame, []byte(`"seq":0`), []byte(`"seq":0,"seq":0`), 1)
	if bytes.Equal(duplicate, input.Frame) {
		return fmt.Errorf("fixture sequence missing")
	}
	if _, err := receive.Open(duplicate); err == nil {
		return fmt.Errorf("duplicate sequence accepted")
	}
	var altered map[string]any
	if err := json.Unmarshal(input.Frame, &altered); err != nil {
		return err
	}
	altered["seq"] = 1
	tampered, err := json.Marshal(altered)
	if err != nil {
		return err
	}
	if _, err := receive.Open(tampered); err == nil {
		return fmt.Errorf("out-of-order frame accepted")
	}
	plain, err := receive.Open(input.Frame)
	if err != nil || string(plain) != `{"type":"ping"}` {
		return fmt.Errorf("mobile frame did not decrypt")
	}
	if _, err := receive.Open(input.Frame); err == nil {
		return fmt.Errorf("replayed frame accepted")
	}
	reply, err := send.Seal([]byte(`{"type":"ready"}`))
	if err != nil {
		return err
	}
	_, err = os.Stdout.Write(reply)
	return err
}

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
