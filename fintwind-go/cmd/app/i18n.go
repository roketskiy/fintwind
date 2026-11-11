package main

import (
	"embed"
	"encoding/json"
	"fmt"
	"strings"
	"sync/atomic"
)

//go:embed locales/*.json
var localeFiles embed.FS

type translationCatalog map[string]string

var activeCatalog atomic.Pointer[translationCatalog]

var detectedSystemLanguage = systemLocaleKey()

var catalogs = func() map[string]translationCatalog {
	result := make(map[string]translationCatalog, 3)
	for _, language := range []string{"en", "zh-CN", "ja"} {
		data, err := localeFiles.ReadFile("locales/" + language + ".json")
		if err != nil {
			panic(err)
		}
		var catalog translationCatalog
		if err := json.Unmarshal(data, &catalog); err != nil {
			panic(err)
		}
		result[language] = catalog
	}
	return result
}()

// SetLanguage selects the app's canonical language value. "system" resolves
// once during package initialization; callers may set it again when preferences load.
func SetLanguage(language string) {
	key := localeKey(language)
	catalog := catalogs[key]
	activeCatalog.Store(&catalog)
}

func init() { SetLanguage("system") }

func localeKey(language string) string {
	switch strings.ToLower(strings.TrimSpace(language)) {
	case "system":
		return detectedSystemLanguage
	case "english", "en", "en-us", "en-gb":
		return "en"
	case "simplified-chinese", "zh", "zh-cn", "chinese":
		return "zh-CN"
	case "japanese", "ja", "ja-jp":
		return "ja"
	default:
		return "en"
	}
}

// tr uses the English UI string as a stable lookup key and always has an English fallback.
func tr(source string) string {
	catalog := activeCatalog.Load()
	if catalog == nil {
		return source
	}
	if translated, ok := (*catalog)[source]; ok && translated != "" {
		return translated
	}
	return source
}

// trf translates a format only when its argument count and types match the format.
// Invalid formatting returns the translated template unchanged, never a %! diagnostic.
func trf(format string, args ...any) string {
	translated := tr(format)
	formatted := fmt.Sprintf(translated, args...)
	if strings.Contains(formatted, "%!") {
		return translated
	}
	return formatted
}
