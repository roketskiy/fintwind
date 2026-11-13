package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"syscall"
	"time"
	"unsafe"

	"fintwind-go/internal/ownedprocess"
	"golang.org/x/sys/windows"
)

type shimEscapeCase struct {
	Name           string   `json:"name"`
	EscapeLayers   int      `json:"escapeLayers"`
	Forward        string   `json:"forward"`
	Script         string   `json:"script"`
	CommandPayload string   `json:"commandPayload"`
	RawCommandLine string   `json:"rawCommandLine"`
	ExpectedArgs   []string `json:"expectedArgs"`
	ReceivedArgs   []string `json:"receivedArgs"`
	Marker         string   `json:"marker"`
	MarkerCreated  bool     `json:"markerCreated"`
	ExitCode       uint32   `json:"exitCode"`
	Stdout         string   `json:"stdout,omitempty"`
	Stderr         string   `json:"stderr,omitempty"`
	Passed         bool     `json:"passed"`
	Error          string   `json:"error,omitempty"`
}

type shimEscapeReport struct {
	Round  string           `json:"round"`
	Cases  []shimEscapeCase `json:"cases"`
	Passed bool             `json:"passed"`
}

func runShimEscapeDiagnostic(ctx context.Context, outputDir string) error {
	if outputDir == "" {
		outputDir = filepath.Join("..", "target", "go-backend-claudequota", "reports")
	}
	outputDir, err := filepath.Abs(outputDir)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(outputDir, 0o700); err != nil {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	round, err := os.MkdirTemp(outputDir, "round-claude-shim-escape-*")
	if err != nil {
		return err
	}
	h := &harness{
		ctx: ctx, round: round,
		dataDir: filepath.Join(round, "data"), home: filepath.Join(round, "home"),
		workspace: filepath.Join(round, "workspace", "fintwind-claude-quota"),
		bin:       filepath.Join(round, "bin"), self: self,
		checks: map[string]bool{}, notes: map[string]string{},
	}
	if err := h.prepare(); err != nil {
		return err
	}
	var report shimEscapeReport
	report.Round = round
	for _, escapeLayers := range []int{1, 2} {
		for _, forward := range []string{"numeric", "percent-star"} {
			name := fmt.Sprintf("%s-%s", []string{"single", "double"}[escapeLayers-1], forward)
			result := h.runShimEscapeCase(name, escapeLayers, forward)
			report.Cases = append(report.Cases, result)
		}
	}
	report.Passed = true
	for _, result := range report.Cases {
		if !result.Passed {
			report.Passed = false
		}
	}
	path := filepath.Join(round, "shim-escape-comparison.json")
	body, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		return err
	}
	if err := os.WriteFile(path, body, 0o600); err != nil {
		return err
	}
	fmt.Println("[SHIM-ESCAPE-REPORT]", path)
	for _, result := range report.Cases {
		fmt.Printf("[SHIM-ESCAPE] %s layers=%d forward=%s passed=%v expected=%q received=%q marker=%v err=%s\n",
			result.Name, result.EscapeLayers, result.Forward, result.Passed,
			result.ExpectedArgs, result.ReceivedArgs, result.MarkerCreated, result.Error)
	}
	if !report.Passed {
		return errors.New("one or more owned cmd escape comparison cases failed")
	}
	return nil
}

func (h *harness) runShimEscapeCase(name string, escapeLayers int, forward string) (result shimEscapeCase) {
	result.Name, result.EscapeLayers, result.Forward = name, escapeLayers, forward
	root := filepath.Join(h.round, name)
	config := filepath.Join(root, ".claude")
	for _, dir := range []string{config, filepath.Join(root, "tmp")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			result.Error = err.Error()
			return result
		}
	}
	if err := copyFile(filepath.Join(h.bin, "claude.exe"), filepath.Join(root, "claude.exe")); err != nil {
		result.Error = err.Error()
		return result
	}
	result.Marker = filepath.Join(root, "injected-command.marker")
	args := []string{"", "plain", "two words", "A&B", "100%literal", `quote"value`, "safe & echo INJECTED > \"" + result.Marker + "\" & rem"}
	result.ExpectedArgs = append([]string(nil), args...)
	scriptPath := filepath.Join(root, "claude-shim.cmd")
	forwardArgs := "%*"
	if forward == "numeric" {
		forwardArgs = "%1 %2 %3 %4 %5 %6 %7"
	}
	result.Script = "@echo off\r\nsetlocal DisableDelayedExpansion\r\n\"%~dp0claude.exe\" shim-record " + forwardArgs + "\r\n"
	if err := os.WriteFile(scriptPath, []byte(result.Script), 0o600); err != nil {
		result.Error = err.Error()
		return result
	}
	result.CommandPayload = diagnosticEscapeBatchMeta(scriptPath)
	for _, arg := range args {
		result.CommandPayload += " " + diagnosticQuoteBatchArg(arg, escapeLayers)
	}
	cmdPath := filepath.Join(os.Getenv("SystemRoot"), "System32", "cmd.exe")
	result.RawCommandLine = syscall.EscapeArg(cmdPath) + " /d /e:ON /v:OFF /s /c \"" + result.CommandPayload + "\""
	env := ownedprocess.BuildEnv(root, map[string]string{
		"PATH": h.bin, "HOME": root, "USERPROFILE": root, "CLAUDE_CONFIG_DIR": config,
		"TEMP": filepath.Join(root, "tmp"), "TMP": filepath.Join(root, "tmp"),
	})
	var runErr error
	result.ExitCode, result.Stdout, result.Stderr, runErr = runRawOwnedCmd(h.ctx, cmdPath, result.RawCommandLine, root, env)
	if runErr != nil {
		result.Error = runErr.Error()
		return result
	}
	if calls, callErr := readCalls(config); callErr == nil && len(calls) == 1 {
		result.ReceivedArgs = calls[0].Args
	} else if callErr != nil {
		result.Error = "fake invocation missing: " + callErr.Error()
	} else {
		result.Error = fmt.Sprintf("fake invocation count=%d, want 1", len(calls))
	}
	if _, statErr := os.Stat(result.Marker); statErr == nil {
		result.MarkerCreated = true
	} else if !os.IsNotExist(statErr) && result.Error == "" {
		result.Error = statErr.Error()
	}
	result.Passed = result.Error == "" && result.ExitCode == 0 && !result.MarkerCreated && reflect.DeepEqual(result.ReceivedArgs, result.ExpectedArgs)
	if result.Error == "" && !reflect.DeepEqual(result.ReceivedArgs, result.ExpectedArgs) {
		result.Error = "argv differs from exact expected slice"
	}
	if result.Error == "" && result.MarkerCreated {
		result.Error = "injected-command marker exists"
	}
	return result
}

func diagnosticQuoteBatchArg(arg string, escapeLayers int) string {
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
	value := out.String()
	for range escapeLayers {
		value = diagnosticEscapeBatchMeta(value)
	}
	return value
}

func diagnosticEscapeBatchMeta(value string) string {
	var out strings.Builder
	for _, r := range value {
		if strings.ContainsRune("()[]%!^\"`<>&|;, *?", r) {
			out.WriteByte('^')
		}
		out.WriteRune(r)
	}
	return out.String()
}

type diagnosticJobBasicLimits struct {
	perProcessUserTimeLimit int64
	perJobUserTimeLimit     int64
	limitFlags              uint32
	minimumWorkingSetSize   uintptr
	maximumWorkingSetSize   uintptr
	activeProcessLimit      uint32
	affinityMask            uintptr
	priorityClass           uint32
	schedulingClass         uint32
}

type diagnosticJobExtendedLimits struct {
	basic                diagnosticJobBasicLimits
	ioInfo               [6]uint64
	processMemoryLimit   uintptr
	jobMemoryLimit       uintptr
	peakProcessMemoryUse uintptr
	peakJobMemoryUse     uintptr
}

type diagnosticStartupInfo struct {
	windows.StartupInfo
	procThreadAttributeList *windows.ProcThreadAttributeList
}

func runRawOwnedCmd(ctx context.Context, executable, commandLine, workingDir string, environment []string) (uint32, string, string, error) {
	const (
		createSuspended          = 0x00000004
		createUnicodeEnvironment = 0x00000400
		jobObjectExtendedLimit   = 9
		jobLimitKillOnClose      = 0x00002000
	)
	kernel32 := syscall.NewLazyDLL("kernel32.dll")
	procCreateJobObject := kernel32.NewProc("CreateJobObjectW")
	procSetJobInfo := kernel32.NewProc("SetInformationJobObject")
	procAssignJob := kernel32.NewProc("AssignProcessToJobObject")
	procTerminateJob := kernel32.NewProc("TerminateJobObject")
	procCloseHandle := kernel32.NewProc("CloseHandle")
	app, err := windows.UTF16PtrFromString(executable)
	if err != nil {
		return 0, "", "", err
	}
	cmd, err := windows.UTF16PtrFromString(commandLine)
	if err != nil {
		return 0, "", "", err
	}
	cwd, err := windows.UTF16PtrFromString(workingDir)
	if err != nil {
		return 0, "", "", err
	}
	stdoutPath, stderrPath := filepath.Join(workingDir, "child-stdout.log"), filepath.Join(workingDir, "child-stderr.log")
	stdinPath := filepath.Join(workingDir, "child-stdin.bin")
	stdin, err := os.OpenFile(stdinPath, os.O_CREATE|os.O_EXCL|os.O_RDONLY, 0o600)
	if err != nil {
		return 0, "", "", err
	}
	defer stdin.Close()
	stdout, err := os.OpenFile(stdoutPath, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return 0, "", "", err
	}
	defer stdout.Close()
	stderr, err := os.OpenFile(stderrPath, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return 0, "", "", err
	}
	defer stderr.Close()
	duplicates := make([]windows.Handle, 0, 3)
	duplicate := func(file *os.File) (windows.Handle, error) {
		var handle windows.Handle
		if err := windows.DuplicateHandle(windows.CurrentProcess(), windows.Handle(file.Fd()), windows.CurrentProcess(), &handle, 0, true, windows.DUPLICATE_SAME_ACCESS); err != nil {
			return 0, err
		}
		duplicates = append(duplicates, handle)
		return handle, nil
	}
	inHandle, err := duplicate(stdin)
	if err != nil {
		return 0, "", "", err
	}
	outHandle, err := duplicate(stdout)
	if err != nil {
		return 0, "", "", err
	}
	errHandle, err := duplicate(stderr)
	if err != nil {
		return 0, "", "", err
	}
	defer func() {
		for _, handle := range duplicates {
			_ = windows.CloseHandle(handle)
		}
	}()
	attributes, err := windows.NewProcThreadAttributeList(1)
	if err != nil {
		return 0, "", "", err
	}
	defer attributes.Delete()
	if err := attributes.Update(windows.PROC_THREAD_ATTRIBUTE_HANDLE_LIST, unsafe.Pointer(&duplicates[0]), uintptr(len(duplicates))*unsafe.Sizeof(duplicates[0])); err != nil {
		return 0, "", "", err
	}
	startup := windows.StartupInfoEx{}
	startup.Cb = uint32(unsafe.Sizeof(startup))
	startup.Flags = windows.STARTF_USESTDHANDLES
	startup.StdInput, startup.StdOutput, startup.StdErr = inHandle, outHandle, errHandle
	startup.ProcThreadAttributeList = attributes.List()
	envBlock := diagnosticEnvBlock(environment)
	jobValue, _, errno := procCreateJobObject.Call(0, 0)
	if jobValue == 0 {
		return 0, "", "", fmt.Errorf("CreateJobObjectW: %v", errno)
	}
	job := windows.Handle(jobValue)
	defer procCloseHandle.Call(uintptr(job))
	limits := diagnosticJobExtendedLimits{}
	limits.basic.limitFlags = jobLimitKillOnClose
	ok, _, errno := procSetJobInfo.Call(uintptr(job), jobObjectExtendedLimit, uintptr(unsafe.Pointer(&limits)), unsafe.Sizeof(limits))
	if ok == 0 {
		return 0, "", "", fmt.Errorf("SetInformationJobObject: %v", errno)
	}
	var process windows.ProcessInformation
	flags := uint32(createSuspended | createUnicodeEnvironment | windows.EXTENDED_STARTUPINFO_PRESENT)
	if err := windows.CreateProcess(app, cmd, nil, nil, true, flags, &envBlock[0], cwd, &startup.StartupInfo, &process); err != nil {
		return 0, "", "", fmt.Errorf("CreateProcessW raw cmd line: %w", err)
	}
	defer windows.CloseHandle(process.Thread)
	defer windows.CloseHandle(process.Process)
	if ok, _, errno = procAssignJob.Call(uintptr(job), uintptr(process.Process)); ok == 0 {
		_ = windows.TerminateProcess(process.Process, 1)
		_, _ = windows.WaitForSingleObject(process.Process, 5000)
		return 0, "", "", fmt.Errorf("AssignProcessToJobObject: %v", errno)
	}
	if _, err := windows.ResumeThread(process.Thread); err != nil {
		_, _, _ = procTerminateJob.Call(uintptr(job), 1)
		_, _ = windows.WaitForSingleObject(process.Process, 5000)
		return 0, "", "", fmt.Errorf("ResumeThread: %w", err)
	}
	deadline := time.Now().Add(10 * time.Second)
	for {
		select {
		case <-ctx.Done():
			_, _, _ = procTerminateJob.Call(uintptr(job), 1)
			_, _ = windows.WaitForSingleObject(process.Process, 5000)
			return 0, "", "", ctx.Err()
		default:
		}
		status, err := windows.WaitForSingleObject(process.Process, 50)
		if err != nil {
			_, _, _ = procTerminateJob.Call(uintptr(job), 1)
			_, _ = windows.WaitForSingleObject(process.Process, 5000)
			return 0, "", "", err
		}
		if status == windows.WAIT_OBJECT_0 {
			break
		}
		if time.Now().After(deadline) {
			_, _, _ = procTerminateJob.Call(uintptr(job), 1)
			_, _ = windows.WaitForSingleObject(process.Process, 5000)
			return 0, "", "", errors.New("raw cmd line owned Job exceeded 10s")
		}
	}
	var exitCode uint32
	if err := windows.GetExitCodeProcess(process.Process, &exitCode); err != nil {
		return 0, "", "", err
	}
	_, _, _ = procTerminateJob.Call(uintptr(job), 1)
	_, _ = windows.WaitForSingleObject(process.Process, 5000)
	_ = stdout.Sync()
	_ = stderr.Sync()
	outBytes, outErr := os.ReadFile(stdoutPath)
	errBytes, readErr := os.ReadFile(stderrPath)
	if outErr != nil || readErr != nil {
		return exitCode, string(outBytes), string(errBytes), errors.Join(outErr, readErr)
	}
	return exitCode, string(outBytes), string(errBytes), nil
}

func diagnosticEnvBlock(environment []string) []uint16 {
	var block []uint16
	for _, entry := range environment {
		value, err := windows.UTF16FromString(entry)
		if err == nil {
			block = append(block, value...)
		}
	}
	block = append(block, 0)
	return block
}
