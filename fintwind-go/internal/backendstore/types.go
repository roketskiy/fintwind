package backendstore

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"

	"github.com/google/uuid"
)

type Object map[string]json.RawMessage

func Decode(data []byte) (Object, error) {
	var value Object
	if err := json.Unmarshal(data, &value); err != nil || value == nil {
		return nil, errors.New("JSON object required")
	}
	return value, nil
}

func (o Object) String(key string) string {
	var value string
	_ = json.Unmarshal(o[key], &value)
	return value
}

func (o Object) Set(key string, value any) {
	data, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	o[key] = data
}

func (o Object) Int(key string) (int64, error) {
	if len(o[key]) == 0 || bytes.Equal(o[key], []byte("null")) {
		return 0, nil
	}
	var n uint64
	if err := json.Unmarshal(o[key], &n); err != nil || n > math.MaxInt64 {
		return 0, fmt.Errorf("invalid integer field %s", key)
	}
	return int64(n), nil
}

func (o Object) Bool(key string) bool {
	var value bool
	_ = json.Unmarshal(o[key], &value)
	return value
}

func (o Object) Nullable(key string) any {
	if len(o[key]) == 0 || bytes.Equal(o[key], []byte("null")) {
		return nil
	}
	var value string
	if json.Unmarshal(o[key], &value) == nil {
		return value
	}
	return nil
}

func (o Object) ID(key string) (string, error) {
	value := o.String(key)
	id, err := uuid.Parse(value)
	if err != nil || id == uuid.Nil {
		return "", fmt.Errorf("invalid %s", key)
	}
	return id.String(), nil
}

func Reply(kind string, fields Object) Object {
	if fields == nil {
		fields = Object{}
	}
	fields.Set("type", kind)
	return fields
}

func Fields(values map[string]any) Object {
	result := Object{}
	for key, value := range values {
		result.Set(key, value)
	}
	return result
}
