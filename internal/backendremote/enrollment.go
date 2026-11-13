package backendremote

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"
)

const RelayOrigin = "https://fintwind-mobile.roketskiy.top"

type Enrollment struct {
	Origin   string
	Client   *http.Client
	Identity *Identity
}

func (enrollment Enrollment) Token(ctx context.Context) (string, error) {
	if enrollment.Identity == nil || enrollment.Client == nil {
		return "", errors.New("remote enrollment unavailable")
	}
	endpoint, err := url.Parse(enrollment.Origin)
	if err != nil || endpoint.User != nil || endpoint.RawQuery != "" || endpoint.Fragment != "" || (endpoint.Path != "" && endpoint.Path != "/") {
		return "", errors.New("invalid remote enrollment endpoint")
	}
	if enrollment.Origin != RelayOrigin {
		ip := net.ParseIP(endpoint.Hostname())
		if endpoint.Scheme != "http" || ip == nil || !ip.IsLoopback() {
			return "", errors.New("invalid remote enrollment endpoint")
		}
	}
	var challenge struct {
		Nonce     string `json:"nonce"`
		ExpiresAt int64  `json:"expiresAt"`
	}
	publicKey := enrollment.Identity.PublicKey()
	if err := enrollment.post(ctx, "/v1/admission/challenge", map[string]string{"publicKey": publicKey}, &challenge); err != nil {
		return "", err
	}
	now := time.Now().Unix()
	if challenge.ExpiresAt <= now || challenge.ExpiresAt > now+90 {
		return "", errors.New("invalid remote enrollment challenge")
	}
	signature, err := enrollment.Identity.SignAdmission(RelayOrigin, challenge.Nonce)
	if err != nil {
		return "", err
	}
	var response struct {
		Token          string `json:"token"`
		InstallationID string `json:"installationId"`
		ExpiresAt      int64  `json:"expiresAt"`
	}
	if err := enrollment.post(ctx, "/v1/admission/token", map[string]string{"publicKey": publicKey, "nonce": challenge.Nonce, "signature": signature}, &response); err != nil {
		return "", err
	}
	if len(response.Token) > 4096 || response.InstallationID != enrollment.Identity.InstallationID() {
		return "", errors.New("invalid remote enrollment response")
	}
	parts := strings.Split(response.Token, ".")
	if len(parts) != 3 {
		return "", errors.New("invalid remote enrollment response")
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return "", errors.New("invalid remote enrollment response")
	}
	var claims struct {
		Subject  string `json:"sub"`
		Issuer   string `json:"iss"`
		Audience string `json:"aud"`
		IssuedAt int64  `json:"iat"`
		Expires  int64  `json:"exp"`
	}
	if json.Unmarshal(payload, &claims) != nil || claims.Subject != response.InstallationID || claims.Issuer != "fintwind-relay" || claims.Audience != "alma-relay" || claims.Expires != response.ExpiresAt || claims.Expires <= now+30 || claims.IssuedAt > now+60 || claims.Expires <= claims.IssuedAt || claims.Expires-claims.IssuedAt > 86460 {
		return "", errors.New("invalid remote enrollment response")
	}
	return response.Token, nil
}

func (enrollment Enrollment) post(ctx context.Context, path string, body any, target any) error {
	raw, err := json.Marshal(body)
	if err != nil {
		return err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, enrollment.Origin+path, bytes.NewReader(raw))
	if err != nil {
		return errors.New("remote enrollment unavailable")
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Accept", "application/json")
	client := *enrollment.Client
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return errors.New("remote enrollment redirect rejected") }
	response, err := client.Do(request)
	if err != nil {
		return errors.New("remote enrollment unavailable")
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return errors.New("remote enrollment rejected")
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, 16385))
	if err != nil || len(data) > 16384 || json.Unmarshal(data, target) != nil {
		return errors.New("invalid remote enrollment response")
	}
	return nil
}
