package backendremote

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/http"
	"path/filepath"
	"sync"
	"time"
)

type Activation struct {
	url    string
	cancel context.CancelFunc
	done   chan struct{}
	once   sync.Once
}

func Start(ctx context.Context, home string, handle Handler, status func(string)) (*Activation, error) {
	if home == "" || handle == nil {
		return nil, errors.New("remote control is unavailable")
	}
	var route, secret [32]byte
	if _, err := rand.Read(route[:]); err != nil {
		return nil, err
	}
	if _, err := rand.Read(secret[:]); err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(ctx)
	activation := &Activation{
		url:    RelayOrigin + "/#v=1&route=" + base64.RawURLEncoding.EncodeToString(route[:]) + "&key=" + base64.RawURLEncoding.EncodeToString(secret[:]),
		cancel: cancel,
		done:   make(chan struct{}),
	}
	go func() {
		defer close(activation.done)
		identity, err := LoadIdentity(filepath.Join(home, ".fintwind"))
		if err != nil {
			if ctx.Err() == nil && status != nil {
				status("identityError")
			}
			return
		}
		client := &http.Client{Timeout: 15 * time.Second}
		enrollment := Enrollment{Origin: RelayOrigin, Client: client, Identity: identity}
		tunnel := &Tunnel{Origin: RelayOrigin, Route: route, Secret: secret, Handle: func(requestCtx context.Context, request json.RawMessage) (any, error) {
			if ctx.Err() != nil {
				return nil, context.Canceled
			}
			return handle(requestCtx, request)
		}, Status: func(value string) {
			if ctx.Err() == nil && status != nil {
				status(value)
			}
		}}
		delay := time.Second
		for ctx.Err() == nil {
			if status != nil {
				status("connecting")
			}
			token, err := enrollment.Token(ctx)
			if err == nil {
				tunnel.Token = token
				err = tunnel.Run(ctx)
			}
			if ctx.Err() != nil {
				return
			}
			if status != nil {
				status("failed")
			}
			timer := time.NewTimer(delay)
			select {
			case <-ctx.Done():
				timer.Stop()
				return
			case <-timer.C:
			}
			delay = min(delay*2, 30*time.Second)
		}
	}()
	return activation, nil
}

func (activation *Activation) PairingURL() string {
	return activation.url
}

func (activation *Activation) Stop() {
	activation.once.Do(activation.cancel)
}

func (activation *Activation) Done() <-chan struct{} {
	return activation.done
}
