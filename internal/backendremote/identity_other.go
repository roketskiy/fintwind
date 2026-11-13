//go:build !windows

package backendremote

func protectIdentity(data []byte) ([]byte, error) { return data, nil }

func unprotectIdentity(data []byte) ([]byte, error) { return data, nil }
