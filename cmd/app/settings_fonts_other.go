//go:build !windows

package main

// systemFontFamilies lists installed font families. Only Windows enumerates
// via the registry; elsewhere the settings menus fall back to fixed lists.
func systemFontFamilies() []string {
	return nil
}
