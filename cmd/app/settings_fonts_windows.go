//go:build windows

package main

import (
	"sort"
	"strings"
	"sync"

	"golang.org/x/sys/windows/registry"
)

// systemFontFamilies lists installed font families, sorted and deduplicated,
// mirroring the Rust settings menus that enumerate the text system's fonts.
// Results are cached per process; a mid-run font install shows after restart.
func systemFontFamilies() []string {
	systemFontsOnce.Do(func() {
		systemFontsCache = enumWindowsFontFamilies()
	})
	return systemFontsCache
}

var (
	systemFontsOnce  sync.Once
	systemFontsCache []string
)

// fontRegistryKey holds the display-name to file mapping Windows maintains
// for installed fonts, per machine and per user.
const fontRegistryKey = `SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts`

func enumWindowsFontFamilies() []string {
	seen := map[string]bool{}
	var names []string
	for _, hive := range []registry.Key{registry.LOCAL_MACHINE, registry.CURRENT_USER} {
		key, err := registry.OpenKey(hive, fontRegistryKey, registry.QUERY_VALUE)
		if err != nil {
			continue
		}
		values, err := key.ReadValueNames(0)
		key.Close()
		if err != nil {
			continue
		}
		for _, display := range values {
			// Combined entries ("Microsoft YaHei & Microsoft YaHei UI")
			// share one file between two families; parse each side.
			for _, part := range strings.Split(display, " & ") {
				family := fontFamilyFromDisplayName(part)
				// "@"-prefixed entries are vertical variants, unusable as UI fonts.
				if family == "" || family[0] == '@' || seen[family] {
					continue
				}
				seen[family] = true
				names = append(names, family)
			}
		}
	}
	sort.Strings(names)
	return names
}

// fontStyleSuffixes are trailing style words stripped from registry display
// names to recover the family ("Arial Bold Italic" -> "Arial"). Weight and
// slope only: width words ("Narrow") and names like "Times New Roman" or
// "Bookman Old Style" are families in their own right and stay whole.
// Matching is longest-first and case-insensitive; single-word names are
// never stripped. Display names are heuristics, not the font name table,
// so rare families can still come out approximate.
var fontStyleSuffixes = []string{
	"Bold Italic", "SemiBold Italic", "DemiBold Italic", "Medium Italic",
	"Light Italic", "ExtraLight Italic", "ExtraBold Italic", "Black Italic",
	"Italic", "Oblique",
	"SemiBold", "DemiBold", "ExtraBold", "UltraBold",
	"Medium", "Demi", "Light", "SemiLight", "ExtraLight", "UltraLight", "Thin",
	"Bold", "Black", "Heavy", "Regular",
}

func fontFamilyFromDisplayName(display string) string {
	name := display
	if i := strings.LastIndex(name, " ("); i >= 0 {
		name = name[:i]
	}
	name = strings.TrimSpace(name)
	for {
		stripped := false
		lower := strings.ToLower(name)
		for _, suffix := range fontStyleSuffixes {
			if len(name) <= len(suffix) || !strings.HasSuffix(lower, strings.ToLower(suffix)) {
				continue
			}
			if name[len(name)-len(suffix)-1] != ' ' {
				continue
			}
			name = strings.TrimSpace(name[:len(name)-len(suffix)])
			stripped = true
			break
		}
		if !stripped {
			break
		}
	}
	return name
}
