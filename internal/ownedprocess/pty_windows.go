//go:build windows

package ownedprocess

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

// PTY is a real Windows pseudo-console. Its reader always drains the console,
// including during Close; this is required to avoid ClosePseudoConsole waiting
// on a full output pipe. Output is bounded and never written to a log file.
type PTY struct {
	Owned   *Owned
	Output  <-chan []byte
	input   *os.File
	reader  *os.File
	console windows.Handle
	stop    chan struct{}
	done    chan struct{}
	once    sync.Once
	mu      sync.Mutex
	err     error
}

func (p *PTY) Err() error              { p.mu.Lock(); defer p.mu.Unlock(); return p.err }
func (p *PTY) Write(data []byte) error { _, err := p.input.Write(data); return err }
func (p *PTY) Close() error {
	p.once.Do(func() {
		close(p.stop)
		_ = p.input.Close()
		if p.Owned != nil {
			err := p.Owned.Close()
			p.mu.Lock()
			if p.err == nil {
				p.err = err
			}
			p.mu.Unlock()
		}
		windows.ClosePseudoConsole(p.console)
		_ = p.reader.Close()
		<-p.done
	})
	return p.Err()
}

// StartPTY does not inherit the caller's console or stdio handles. The process
// cannot execute until it belongs to this invocation's kill-on-close Job.
func StartPTY(executable string, args []string, runDir, workingDir string, environment []string) (result *PTY, err error) {
	if !filepath.IsAbs(executable) || !filepath.IsAbs(runDir) || !filepath.IsAbs(workingDir) || len(environment) == 0 {
		return nil, errors.New("ownedprocess: absolute PTY paths and explicit environment required")
	}
	application, line, err := ptyCommand(executable, args)
	if err != nil {
		return nil, err
	}
	app, err := windows.UTF16PtrFromString(application)
	if err != nil {
		return nil, err
	}
	cmd, err := windows.UTF16PtrFromString(line)
	if err != nil {
		return nil, err
	}
	cwd, err := windows.UTF16PtrFromString(workingDir)
	if err != nil {
		return nil, err
	}
	inRead, inWrite, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	defer inRead.Close()
	outRead, outWrite, err := os.Pipe()
	if err != nil {
		inWrite.Close()
		return nil, err
	}
	defer outWrite.Close()
	var console windows.Handle
	if err = windows.CreatePseudoConsole(windows.Coord{X: 200, Y: 50}, windows.Handle(inRead.Fd()), windows.Handle(outWrite.Fd()), 0, &console); err != nil {
		inWrite.Close()
		outRead.Close()
		return nil, err
	}
	chunks := make(chan []byte, 32)
	p := &PTY{input: inWrite, reader: outRead, console: console, stop: make(chan struct{}), done: make(chan struct{}), Output: chunks}
	go func() {
		defer close(p.done)
		defer close(chunks)
		buffer := make([]byte, 4096)
		total := 0
		discard := false
		for {
			n, readErr := outRead.Read(buffer)
			if n > 0 {
				total += n
				select {
				case <-p.stop:
					discard = true
				default:
				}
				if total > 1<<20 && !discard {
					p.mu.Lock()
					p.err = errors.New("ownedprocess: PTY output exceeded budget")
					p.mu.Unlock()
					discard = true
				}
				if !discard {
					copyBytes := append([]byte(nil), buffer[:n]...)
					select {
					case chunks <- copyBytes:
					case <-p.stop:
						discard = true
					default:
						p.mu.Lock()
						p.err = errors.New("ownedprocess: PTY consumer fell behind")
						p.mu.Unlock()
						discard = true
					}
				}
			}
			if readErr != nil {
				return
			}
		}
	}()
	defer func() {
		if err != nil {
			_ = p.Close()
		}
	}()
	job, _, errno := procCreateJobObjectW.Call(0, 0)
	if job == 0 {
		return nil, fmt.Errorf("ownedprocess: PTY Job creation: %v", errno)
	}
	limits := jobExtendedLimits{}
	limits.basic.limitFlags = jobObjectLimitKillOnJobClose
	ok, _, errno := procSetInformationJobObject.Call(job, jobObjectExtendedLimit, uintptr(unsafe.Pointer(&limits)), unsafe.Sizeof(limits))
	if ok == 0 {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: PTY Job limits: %v", errno)
	}
	attributes, e := windows.NewProcThreadAttributeList(1)
	if e != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, e
	}
	defer attributes.Delete()
	// PSEUDOCONSOLE takes the opaque console value itself, not &console.
	update := kernel32.NewProc("UpdateProcThreadAttribute")
	ok, _, errno = update.Call(uintptr(unsafe.Pointer(attributes.List())), 0, windows.PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, uintptr(console), unsafe.Sizeof(console), 0, 0)
	if ok == 0 {
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: attach PTY: %v", errno)
	}
	startup := windows.StartupInfoEx{}
	startup.Cb = uint32(unsafe.Sizeof(startup))
	startup.Flags = windows.STARTF_USESTDHANDLES
	startup.ProcThreadAttributeList = attributes.List()
	var process windows.ProcessInformation
	env := envBlock(environment)
	err = windows.CreateProcess(app, cmd, nil, nil, false, createSuspended|createUnicodeEnvironment|windows.EXTENDED_STARTUPINFO_PRESENT, &env[0], cwd, &startup.StartupInfo, &process)
	if err != nil {
		_, _, _ = procCloseHandle.Call(job)
		return nil, err
	}
	defer windows.CloseHandle(process.Thread)
	ok, _, errno = procAssignProcessToJobObject.Call(job, uintptr(process.Process))
	if ok == 0 {
		_ = windows.TerminateProcess(process.Process, 1)
		_, _ = windows.WaitForSingleObject(process.Process, 5000)
		windows.CloseHandle(process.Process)
		_, _, _ = procCloseHandle.Call(job)
		return nil, fmt.Errorf("ownedprocess: PTY Job assignment: %v", errno)
	}
	owned := &Owned{PID: process.ProcessId, Dir: runDir}
	owned.close = func() error {
		_, _, _ = procTerminateJobObject.Call(job, 1)
		status, waitErr := windows.WaitForSingleObject(process.Process, 5000)
		_ = windows.CloseHandle(process.Process)
		_, _, _ = procCloseHandle.Call(job)
		if waitErr != nil {
			return waitErr
		}
		if status != windows.WAIT_OBJECT_0 {
			return errors.New("ownedprocess: PTY process cleanup timed out")
		}
		return nil
	}
	owned.exited = func(ms uint32) (bool, error) {
		status, e := windows.WaitForSingleObject(process.Process, ms)
		if e != nil {
			return false, e
		}
		return status == windows.WAIT_OBJECT_0, nil
	}
	owned.exitCode = func() (uint32, error) {
		var code uint32
		e := windows.GetExitCodeProcess(process.Process, &code)
		return code, e
	}
	p.Owned = owned
	if _, err = windows.ResumeThread(process.Thread); err != nil {
		return nil, err
	}
	return p, nil
}

func ptyCommand(executable string, args []string) (string, string, error) {
	if strings.ContainsAny(executable, "\"\r\n\x00") {
		return "", "", errors.New("ownedprocess: invalid PTY executable")
	}
	for _, arg := range args {
		if strings.ContainsAny(arg, "\r\n\x00") {
			return "", "", errors.New("ownedprocess: invalid PTY argument")
		}
	}
	ext := strings.ToLower(filepath.Ext(executable))
	if ext != ".cmd" && ext != ".bat" {
		return executable, commandLine(executable, args), nil
	}
	if target, forwarded, ok := transparentNumberedShim(executable, args); ok {
		return target, commandLine(target, forwarded), nil
	}
	shell := os.Getenv("COMSPEC")
	if !filepath.IsAbs(shell) || !strings.EqualFold(filepath.Base(shell), "cmd.exe") {
		shell = filepath.Join(os.Getenv("SystemRoot"), "System32", "cmd.exe")
	}
	if !filepath.IsAbs(shell) || strings.ContainsAny(shell, "\"\r\n\x00") || strings.ContainsAny(executable, "%!") {
		return "", "", errors.New("ownedprocess: unsafe PTY shim path")
	}
	line := syscall.EscapeArg(shell) + " /d /e:ON /v:OFF /s /c \"" + escapeBatchMeta(executable)
	for _, arg := range args {
		line += " " + quoteBatchArg(arg)
	}
	return shell, line + "\"", nil
}

// A transparent numbered wrapper expands %1 separately from %*, losing CMD
// escape layers around embedded quotes. Recognize only a complete, inert
// wrapper grammar and invoke its sibling EXE with native argv instead. Any
// script with conditionals, variables, extra commands or nonliteral prefixes
// stays on the normal batch path; it is never partially interpreted here.
func transparentNumberedShim(path string, args []string) (string, []string, bool) {
	file, err := os.Open(path)
	if err != nil {
		return "", nil, false
	}
	defer file.Close()
	raw, err := io.ReadAll(io.LimitReader(file, (64<<10)+1))
	if err != nil || len(raw) > 64<<10 {
		return "", nil, false
	}
	command := ""
	for _, line := range strings.Split(strings.TrimPrefix(string(raw), "\ufeff"), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		switch strings.ToLower(line) {
		case "@echo off", "setlocal disabledelayedexpansion":
			continue
		}
		if command != "" {
			return "", nil, false
		}
		command = line
	}
	const prefix = `"%~dp0`
	if !strings.HasPrefix(command, prefix) {
		return "", nil, false
	}
	name, tail, ok := strings.Cut(strings.TrimPrefix(command, prefix), `"`)
	if !ok || !strings.EqualFold(filepath.Ext(name), ".exe") || filepath.Base(name) != name || strings.ContainsAny(name, "%!:*?<>|/\\\r\n\x00") {
		return "", nil, false
	}
	fields := strings.Fields(tail)
	first := -1
	for i, field := range fields {
		if strings.HasPrefix(field, "%") {
			first = i
			break
		}
		if strings.ContainsAny(field, "\"'&|<>^%!`\x00") {
			return "", nil, false
		}
	}
	if first < 0 {
		return "", nil, false
	}
	count := len(fields) - first
	if count < 1 || count > 9 {
		return "", nil, false
	}
	for i, field := range fields[first:] {
		if field != fmt.Sprintf("%%%d", i+1) {
			return "", nil, false
		}
	}
	target := filepath.Join(filepath.Dir(path), name)
	info, err := os.Stat(target)
	if err != nil || info.IsDir() {
		return "", nil, false
	}
	forwarded := append([]string{}, fields[:first]...)
	forwarded = append(forwarded, args[:min(count, len(args))]...)
	return target, forwarded, true
}
func quoteBatchArg(arg string) string {
	// CMD parses a batch invocation twice (the launch and %* forwarding).
	// Apply CRT quoting first, then protect the CMD metacharacters in both
	// layers. Based on node-cross-spawn (MIT); see docs/licenses/.
	var out strings.Builder
	out.WriteByte('"')
	slashes := 0
	for _, r := range arg {
		if r == '\\' {
			slashes++
			continue
		}
		if r == '"' {
			out.WriteString(strings.Repeat("\\", slashes*2+1))
		} else {
			out.WriteString(strings.Repeat("\\", slashes))
		}
		slashes = 0
		out.WriteRune(r)
	}
	out.WriteString(strings.Repeat("\\", slashes*2))
	out.WriteByte('"')
	return escapeBatchMeta(escapeBatchMeta(out.String()))
}

func escapeBatchMeta(value string) string {
	var out strings.Builder
	for _, r := range value {
		if strings.ContainsRune("()[]%!^\"`<>&|;, *?", r) {
			out.WriteByte('^')
		}
		out.WriteRune(r)
	}
	return out.String()
}
