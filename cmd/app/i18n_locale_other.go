//go:build !windows

package main

import (
	"os"
	"strings"
)

func systemLocaleKey() string {
	for _, key := range []string{"LC_ALL", "LC_MESSAGES", "LANG"} {
		if value := strings.ToLower(os.Getenv(key)); value != "" {
			if strings.HasPrefix(value, "zh") {
				return "zh-CN"
			}
			if strings.HasPrefix(value, "ja") {
				return "ja"
			}
			return "en"
		}
	}
	return "en"
}
