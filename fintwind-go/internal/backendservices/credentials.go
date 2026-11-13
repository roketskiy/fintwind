package backendservices

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"strings"

	"fintwind-go/internal/backendstore"

	"github.com/google/uuid"
)

func credentialRows(ctx context.Context, api API) ([]backendstore.Object, error) {
	raw, err := api.RoundTrip(ctx, "GET", "/api/credential", nil)
	if err != nil {
		return nil, errors.New("could not read provider accounts")
	}
	value, err := data(raw)
	if err != nil {
		return nil, err
	}
	var rows []backendstore.Object
	err = json.Unmarshal(value, &rows)
	return rows, err
}

func storeCredential(ctx context.Context, api API, provider, key string) error {
	if provider == "" || strings.TrimSpace(key) == "" {
		return errors.New("provider and API key required")
	}
	rows, err := credentialRows(ctx, api)
	if err != nil {
		return err
	}
	prefix := "fintwind-custom:" + provider + ":"
	id := ""
	owned := []string{}
	for _, row := range rows {
		secret, _ := backendstore.Decode(row["value"])
		if row.String("integrationID") == provider && strings.HasPrefix(row.String("id"), prefix) && secret.String("type") == "key" {
			owned = append(owned, row.String("id"))
			if secret.String("key") == key {
				id = row.String("id")
			}
		}
	}
	if id == "" {
		id = prefix + uuid.NewString()
		if _, err := api.RoundTrip(ctx, "POST", "/api/credential", map[string]any{"id": id, "integrationID": provider, "label": "Fintwind", "value": map[string]string{"type": "key", "key": key}, "activate": true}); err != nil {
			return errors.New("could not save provider account")
		}
	} else {
		if _, err := api.RoundTrip(ctx, "POST", "/api/credential/"+url.PathEscape(id)+"/activate", nil); err != nil {
			return errors.New("could not activate provider account")
		}
	}
	verified, err := credentialRows(ctx, api)
	if err != nil {
		return err
	}
	confirmed := false
	for _, row := range verified {
		secret, _ := backendstore.Decode(row["value"])
		if row.String("id") == id && row.String("integrationID") == provider && row.Bool("active") && secret.String("type") == "key" && secret.String("key") == key {
			confirmed = true
		}
	}
	if !confirmed {
		return errors.New("server did not confirm provider account")
	}
	for _, previous := range owned {
		if previous != id {
			if _, err := api.RoundTrip(ctx, "DELETE", "/api/credential/"+url.PathEscape(previous), nil); err != nil {
				return errors.New("could not remove replaced provider account")
			}
		}
	}
	return nil
}
