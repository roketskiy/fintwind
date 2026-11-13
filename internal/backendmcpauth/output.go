package backendmcpauth

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"net/url"
	"regexp"
	"strings"
	"sync/atomic"
	"unicode"
)

type outputEvent struct {
	line string
	err  error
	done bool
}

func readOutput(reader io.Reader, events chan<- outputEvent, total *atomic.Int64) {
	defer func() { events <- outputEvent{done: true} }()
	buffer := bufio.NewReaderSize(reader, 16<<10)
	line := []byte{}
	dropping := false
	read := 0
	for {
		part, err := buffer.ReadSlice('\n')
		read += len(part)
		if read > 1<<20 || total.Add(int64(len(part))) > 2<<20 {
			events <- outputEvent{err: errors.New("MCP authorization output exceeded its budget")}
			return
		}
		if !dropping {
			if len(line)+len(part) > 64<<10 {
				line = nil
				dropping = true
			} else {
				line = append(line, part...)
			}
		}
		if err != bufio.ErrBufferFull {
			if !dropping && len(line) > 0 {
				events <- outputEvent{line: string(bytes.TrimRight(line, "\r\n"))}
			}
			line = nil
			dropping = false
		}
		if err != nil && err != bufio.ErrBufferFull {
			if err != io.EOF {
				events <- outputEvent{err: errors.New("MCP authorization output stream failed")}
			}
			return
		}
	}
}

var csi = regexp.MustCompile("\x1b\\[[0-?]*[ -/]*[@-~]")
var oscLink = regexp.MustCompile("\x1b\\]8;;([^\x07\x1b]*)(?:\x07|\x1b\\\\)")
var osc = regexp.MustCompile("\x1b\\][^\x07\x1b]*(?:\x07|\x1b\\\\)")

func authorizationURL(line string) string {
	line = oscLink.ReplaceAllString(line, " $1 ")
	line = osc.ReplaceAllString(line, "")
	line = csi.ReplaceAllString(line, "")
	for {
		index := strings.Index(line, "https://")
		if index < 0 {
			return ""
		}
		candidate := line[index:]
		if end := strings.IndexFunc(candidate, func(r rune) bool {
			return unicode.IsSpace(r) || unicode.IsControl(r) || strings.ContainsRune("\"'<>`", r)
		}); end >= 0 {
			candidate = candidate[:end]
		}
		line = line[index+len("https://"):]
		candidate = strings.TrimRight(candidate, ".,;)]}")
		if len(candidate) > 4096 {
			continue
		}
		parsed, err := url.Parse(candidate)
		if err == nil && parsed.Scheme == "https" && parsed.Hostname() != "" && parsed.User == nil {
			return candidate
		}
	}
}
