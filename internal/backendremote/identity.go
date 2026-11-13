package backendremote

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
)

const identityName = "remote-control-identity.bin"

type Identity struct {
	private ed25519.PrivateKey
	public  ed25519.PublicKey
}

func LoadIdentity(directory string) (*Identity, error) {
	path := filepath.Join(directory, identityName)
	identity, err := readIdentity(path)
	if !errors.Is(err, os.ErrNotExist) {
		return identity, err
	}
	_, key, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return nil, err
	}
	der, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return nil, err
	}
	protected, err := protectIdentity(der)
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(directory, 0700); err != nil {
		return nil, err
	}
	file, err := os.CreateTemp(directory, ".remote-identity-*.tmp")
	if err != nil {
		return nil, err
	}
	defer os.Remove(file.Name())
	if err := file.Chmod(0600); err != nil {
		file.Close()
		return nil, err
	}
	if _, err := file.Write(protected); err != nil {
		file.Close()
		return nil, err
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return nil, err
	}
	if err := file.Close(); err != nil {
		return nil, err
	}
	if err := os.Link(file.Name(), path); err != nil && !errors.Is(err, os.ErrExist) {
		return nil, err
	}
	return readIdentity(path)
}

func readIdentity(path string) (*Identity, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Size() < 1 || info.Size() > 4096 {
		return nil, errors.New("invalid remote identity")
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	opened, err := file.Stat()
	if err != nil || !os.SameFile(info, opened) {
		return nil, errors.New("invalid remote identity")
	}
	stored, err := io.ReadAll(io.LimitReader(file, 4097))
	if err != nil || len(stored) == 0 || len(stored) > 4096 {
		return nil, errors.New("invalid remote identity")
	}
	der, err := unprotectIdentity(stored)
	if err != nil {
		return nil, errors.New("remote identity unavailable")
	}
	parsed, err := x509.ParsePKCS8PrivateKey(der)
	if err != nil {
		return nil, errors.New("invalid remote identity")
	}
	key, ok := parsed.(ed25519.PrivateKey)
	if !ok || len(key) != ed25519.PrivateKeySize {
		return nil, errors.New("invalid remote identity")
	}
	return &Identity{private: key, public: key.Public().(ed25519.PublicKey)}, nil
}

func (identity *Identity) PublicKey() string {
	return base64.RawURLEncoding.EncodeToString(identity.public)
}

func (identity *Identity) InstallationID() string {
	digest := sha256.Sum256(identity.public)
	return hex.EncodeToString(digest[:])
}

func (identity *Identity) SignAdmission(origin, nonce string) (string, error) {
	value, err := base64.RawURLEncoding.DecodeString(nonce)
	if err != nil || len(value) != 32 || base64.RawURLEncoding.EncodeToString(value) != nonce {
		return "", errors.New("invalid admission challenge")
	}
	message := fmt.Sprintf("fintwind.relay.admission.v1\n%s\n%s\n%s", origin, nonce, identity.PublicKey())
	return base64.RawURLEncoding.EncodeToString(ed25519.Sign(identity.private, []byte(message))), nil
}
