package backendremote

import (
	"errors"
	"unsafe"

	"golang.org/x/sys/windows"
)

func protectIdentity(data []byte) ([]byte, error) {
	return cryptIdentity(data, true)
}

func unprotectIdentity(data []byte) ([]byte, error) {
	return cryptIdentity(data, false)
}

func cryptIdentity(data []byte, protect bool) ([]byte, error) {
	if len(data) == 0 {
		return nil, errors.New("empty identity")
	}
	input := windows.DataBlob{Size: uint32(len(data)), Data: &data[0]}
	var output windows.DataBlob
	var err error
	if protect {
		err = windows.CryptProtectData(&input, nil, nil, 0, nil, windows.CRYPTPROTECT_UI_FORBIDDEN, &output)
	} else {
		err = windows.CryptUnprotectData(&input, nil, nil, 0, nil, windows.CRYPTPROTECT_UI_FORBIDDEN, &output)
	}
	if err != nil {
		return nil, err
	}
	defer windows.LocalFree(windows.Handle(unsafe.Pointer(output.Data)))
	if output.Data == nil || output.Size == 0 || output.Size > 4096 {
		return nil, errors.New("invalid protected identity")
	}
	return append([]byte(nil), unsafe.Slice(output.Data, output.Size)...), nil
}
