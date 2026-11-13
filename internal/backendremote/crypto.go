package backendremote

import (
	"bytes"
	"crypto/aes"
	"crypto/cipher"
	"crypto/hkdf"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
)

const maxSequence uint64 = 1<<53 - 1

const frameLimit = 1024 * 1024

var associatedData = []byte("fintwind.remote.v1")

type Direction struct {
	cipher cipher.AEAD
	next   uint64
}

type envelope struct {
	Type string `json:"type"`
	Seq  uint64 `json:"seq"`
	Data string `json:"data"`
}

func Directions(secret, phoneNonce, desktopNonce [32]byte) (send, receive *Direction, err error) {
	salt := append(phoneNonce[:], desktopNonce[:]...)
	client, err := hkdf.Key(sha256.New, secret[:], salt, "fintwind.remote.v1.client", 32)
	if err != nil {
		return nil, nil, err
	}
	server, err := hkdf.Key(sha256.New, secret[:], salt, "fintwind.remote.v1.server", 32)
	if err != nil {
		return nil, nil, err
	}
	newDirection := func(key []byte) (*Direction, error) {
		block, err := aes.NewCipher(key)
		if err != nil {
			return nil, err
		}
		gcm, err := cipher.NewGCM(block)
		if err != nil {
			return nil, err
		}
		return &Direction{cipher: gcm}, nil
	}
	send, err = newDirection(server)
	if err != nil {
		return nil, nil, err
	}
	receive, err = newDirection(client)
	return send, receive, err
}

func (d *Direction) Seal(plaintext []byte) ([]byte, error) {
	if d.next > maxSequence || len(plaintext) > frameLimit/2 {
		return nil, errors.New("remote frame exceeds limit")
	}
	var nonce [12]byte
	binary.BigEndian.PutUint64(nonce[4:], d.next)
	data := d.cipher.Seal(nil, nonce[:], plaintext, associatedData)
	raw, err := json.Marshal(envelope{Type: "sealed", Seq: d.next, Data: base64.RawURLEncoding.EncodeToString(data)})
	if err != nil {
		return nil, err
	}
	d.next++
	return raw, nil
}

func (d *Direction) Open(raw []byte) ([]byte, error) {
	if len(raw) > frameLimit || d.next > maxSequence {
		return nil, errors.New("invalid remote frame")
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.Token()
	if err != nil || start != json.Delim('{') {
		return nil, errors.New("invalid remote frame")
	}
	fields := make(map[string]json.RawMessage, 3)
	for decoder.More() {
		key, err := decoder.Token()
		name, ok := key.(string)
		if err != nil || !ok || fields[name] != nil || (name != "type" && name != "seq" && name != "data") {
			return nil, errors.New("invalid remote frame")
		}
		var value json.RawMessage
		if decoder.Decode(&value) != nil {
			return nil, errors.New("invalid remote frame")
		}
		fields[name] = value
	}
	end, err := decoder.Token()
	if err != nil || end != json.Delim('}') || len(fields) != 3 || decoder.Decode(new(any)) != io.EOF {
		return nil, errors.New("invalid remote frame")
	}
	var frame envelope
	if bytes.Equal(fields["seq"], []byte("null")) || json.Unmarshal(raw, &frame) != nil || frame.Type != "sealed" || frame.Seq != d.next {
		return nil, errors.New("invalid remote frame")
	}
	data, err := base64.RawURLEncoding.DecodeString(frame.Data)
	if err != nil || base64.RawURLEncoding.EncodeToString(data) != frame.Data {
		return nil, errors.New("invalid remote frame")
	}
	var nonce [12]byte
	binary.BigEndian.PutUint64(nonce[4:], frame.Seq)
	plaintext, err := d.cipher.Open(nil, nonce[:], data, associatedData)
	if err != nil {
		return nil, errors.New("invalid remote frame")
	}
	d.next++
	return plaintext, nil
}
