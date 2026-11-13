package backendquota

import (
	"fmt"
	"golang.org/x/text/width"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

// quotaScreen is a bounded VT projection. Cursor moves and erases replace old
// figures rather than appending stale percentage readings to the panel.
type quotaScreen struct {
	grid     [50][200]rune
	row, col int
	escape   string
	pending  []byte
	osc      bool
}

// Forget previous prompt text without inventing a cursor movement: the real
// console cursor stays where the last output left it until a VT command moves it.
func (s *quotaScreen) clear() { s.grid = [50][200]rune{} }
func (s *quotaScreen) text() string {
	lines := make([]string, 50)
	for i, row := range s.grid {
		var b strings.Builder
		for _, r := range row {
			if r == 0 {
				r = ' '
			}
			b.WriteRune(r)
		}
		lines[i] = strings.TrimRight(b.String(), " ")
	}
	return strings.Join(lines, "\n")
}
func (s *quotaScreen) scroll() {
	if s.row >= 50 {
		copy(s.grid[:49], s.grid[1:])
		s.grid[49] = [200]rune{}
		s.row = 49
	}
}
func (s *quotaScreen) feed(raw []byte) string {
	s.pending = append(s.pending, raw...)
	var replies strings.Builder
	for len(s.pending) > 0 {
		if !utf8.FullRune(s.pending) {
			break
		}
		r, n := utf8.DecodeRune(s.pending)
		s.pending = s.pending[n:]
		if s.osc {
			if r == 7 || (s.escape == "\x1b" && r == '\\') {
				s.osc = false
				s.escape = ""
			} else if r == 27 {
				s.escape = "\x1b"
			} else {
				s.escape = ""
			}
			continue
		}
		if s.escape != "" {
			s.escape += string(r)
			if len(s.escape) > 128 {
				s.escape = ""
				continue
			}
			if s.escape == "\x1b]" || s.escape == "\x1bP" {
				s.osc = true
				s.escape = ""
				continue
			}
			if len(s.escape) == 2 && r != '[' {
				s.escape = ""
				continue
			}
			if len(s.escape) > 2 && r >= 0x40 && r <= 0x7e {
				replies.WriteString(s.csi(s.escape[2:len(s.escape)-1], byte(r)))
				s.escape = ""
			}
			continue
		}
		switch r {
		case 27:
			s.escape = "\x1b"
		case '\r':
			s.col = 0
		case '\n':
			s.row++
			s.scroll()
		case '\b':
			if s.col > 0 {
				s.col--
			}
		case '\t':
			s.col = (s.col/8 + 1) * 8
			if s.col >= 200 {
				s.col = 199
			}
		default:
			if r >= 32 && r != 127 {
				if unicode.Is(unicode.Mn, r) || unicode.Is(unicode.Me, r) {
					continue
				}
				cells := 1
				kind := width.LookupRune(r).Kind()
				if kind == width.EastAsianWide || kind == width.EastAsianFullwidth {
					cells = 2
				}
				if s.col >= 200 {
					s.col = 0
					s.row++
					s.scroll()
				}
				s.grid[s.row][s.col] = r
				s.col++
				if cells == 2 && s.col < 200 {
					s.grid[s.row][s.col] = ' '
					s.col++
				}
			}
		}
	}
	return replies.String()
}
func (s *quotaScreen) csi(raw string, op byte) string {
	parts := strings.Split(strings.TrimLeft(raw, "?=>"), ";")
	value := func(index, fallback int) int {
		if index >= len(parts) {
			return fallback
		}
		n, err := strconv.Atoi(parts[index])
		if err != nil || n == 0 {
			return fallback
		}
		if n > 1000 {
			return 1000
		}
		return n
	}
	n := value(0, 1)
	switch op {
	case 'H', 'f':
		s.row = value(0, 1) - 1
		s.col = value(1, 1) - 1
	case 'A':
		s.row -= n
	case 'B':
		s.row += n
	case 'C':
		s.col += n
	case 'D':
		s.col -= n
	case 'G':
		s.col = n - 1
	case 'd':
		s.row = n - 1
	case 'J':
		if raw == "2" || raw == "3" {
			s.grid = [50][200]rune{}
		} else if raw == "1" {
			for y := 0; y < s.row; y++ {
				s.grid[y] = [200]rune{}
			}
			for x := 0; x <= s.col && x < 200; x++ {
				s.grid[s.row][x] = 0
			}
		} else {
			for x := s.col; x < 200; x++ {
				s.grid[s.row][x] = 0
			}
			for y := s.row + 1; y < 50; y++ {
				s.grid[y] = [200]rune{}
			}
		}
	case 'K':
		start, end := s.col, 200
		if raw == "2" {
			start = 0
		} else if raw == "1" {
			start = 0
			end = s.col + 1
		}
		for x := start; x < end && x < 200; x++ {
			s.grid[s.row][x] = 0
		}
	case 'n':
		if raw == "6" {
			return fmt.Sprintf("\x1b[%d;%dR", s.row+1, s.col+1)
		}
		if raw == "5" {
			return "\x1b[0n"
		}
	case 'c':
		return "\x1b[?1;2c"
	}
	if s.row < 0 {
		s.row = 0
	}
	if s.row > 49 {
		s.row = 49
	}
	if s.col < 0 {
		s.col = 0
	}
	if s.col > 199 {
		s.col = 199
	}
	return ""
}
