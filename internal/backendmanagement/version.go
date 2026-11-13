package backendmanagement

import (
	"errors"
	"regexp"
	"strings"
)

var versionPattern = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$`)

type version struct {
	core []string
	pre  []string
}

func numeric(text string) bool {
	if text == "" {
		return false
	}
	for _, r := range text {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}
func parseVersion(text string) (version, error) {
	if len(text) > 256 {
		return version{}, errors.New("version exceeds budget")
	}
	parts := versionPattern.FindStringSubmatch(text)
	if parts == nil {
		return version{}, errors.New("invalid semantic version")
	}
	v := version{core: parts[1:4]}
	if parts[4] != "" {
		v.pre = strings.Split(parts[4], ".")
		for _, part := range v.pre {
			if numeric(part) && len(part) > 1 && part[0] == '0' {
				return version{}, errors.New("invalid numeric prerelease")
			}
		}
	}
	return v, nil
}
func compareNumber(a, b string) int {
	if len(a) < len(b) {
		return -1
	}
	if len(a) > len(b) {
		return 1
	}
	return strings.Compare(a, b)
}
func Compare(a, b string) (int, error) {
	left, err := parseVersion(a)
	if err != nil {
		return 0, err
	}
	right, err := parseVersion(b)
	if err != nil {
		return 0, err
	}
	for i := range left.core {
		if n := compareNumber(left.core[i], right.core[i]); n != 0 {
			return n, nil
		}
	}
	if len(left.pre) == 0 && len(right.pre) == 0 {
		return 0, nil
	}
	if len(left.pre) == 0 {
		return 1, nil
	}
	if len(right.pre) == 0 {
		return -1, nil
	}
	for i := 0; i < min(len(left.pre), len(right.pre)); i++ {
		x, y := left.pre[i], right.pre[i]
		var n int
		if numeric(x) && numeric(y) {
			n = compareNumber(x, y)
		} else if numeric(x) {
			n = -1
		} else if numeric(y) {
			n = 1
		} else {
			n = strings.Compare(x, y)
		}
		if n != 0 {
			return n, nil
		}
	}
	if len(left.pre) < len(right.pre) {
		return -1, nil
	}
	if len(left.pre) > len(right.pre) {
		return 1, nil
	}
	return 0, nil
}

func SupportedVersion(text string) bool {
	v, err := parseVersion(text)
	return err == nil && compareNumber(v.core[0], "2") >= 0
}

func ParseCLI(output []byte) string {
	for _, word := range strings.Fields(string(output)) {
		candidate := strings.Trim(strings.TrimPrefix(word, "v"), "(),;")
		if _, err := parseVersion(candidate); err == nil {
			return candidate
		}
	}
	return ""
}
