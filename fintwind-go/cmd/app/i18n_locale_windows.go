//go:build windows

package main

import (
	"strings"
	"syscall"
	"unsafe"
)

func systemLocaleKey() string {
	getLocale := syscall.NewLazyDLL("kernel32.dll").NewProc("GetUserDefaultLocaleName")
	buffer := make([]uint16, 85)
	result, _, _ := getLocale.Call(uintptr(unsafe.Pointer(&buffer[0])), uintptr(len(buffer)))
	if result == 0 {
		return "en"
	}
	locale := strings.ToLower(syscall.UTF16ToString(buffer))
	if strings.HasPrefix(locale, "zh") {
		return "zh-CN"
	}
	if strings.HasPrefix(locale, "ja") {
		return "ja"
	}
	return "en"
}
