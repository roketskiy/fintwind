//go:build windows

package main

import (
	"errors"
	"math"
	"os"
	"runtime"
	"sort"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

type helpCpuBaseline struct {
	createTime uint64
	cpu100ns   uint64
	at         time.Time
}

type helpTrackedProcess struct {
	parent     uint32
	createTime *uint64
}

type helpProcessSampler struct {
	baselines map[uint32]helpCpuBaseline
	tracked   map[uint32]helpTrackedProcess
}

type helpProcessMetrics struct {
	createTime *uint64
	cpu100ns   *uint64
	workingSet *uint64
	private    *uint64
	ioRead     *uint64
	ioWrite    *uint64
}

type helpSystemProcess struct {
	parent uint32
	name   string
}

type helpProcessMemoryCountersEx struct {
	cb                         uint32
	pageFaultCount             uint32
	peakWorkingSetSize         uintptr
	workingSetSize             uintptr
	quotaPeakPagedPoolUsage    uintptr
	quotaPagedPoolUsage        uintptr
	quotaPeakNonPagedPoolUsage uintptr
	quotaNonPagedPoolUsage     uintptr
	pagefileUsage              uintptr
	peakPagefileUsage          uintptr
	privateUsage               uintptr
}

type helpIOCounters struct {
	readOperationCount  uint64
	writeOperationCount uint64
	otherOperationCount uint64
	readTransferCount   uint64
	writeTransferCount  uint64
	otherTransferCount  uint64
}

var (
	helpKernel32             = windows.NewLazySystemDLL("kernel32.dll")
	helpPsapi                = windows.NewLazySystemDLL("psapi.dll")
	helpGetProcessIoCounters = helpKernel32.NewProc("GetProcessIoCounters")
	helpGetProcessMemoryInfo = helpPsapi.NewProc("GetProcessMemoryInfo")
)

func newHelpProcessSampler() *helpProcessSampler {
	return &helpProcessSampler{
		baselines: map[uint32]helpCpuBaseline{},
		tracked:   map[uint32]helpTrackedProcess{},
	}
}

func (s *helpProcessSampler) sample() (helpProcessSample, error) {
	snapshot, err := windows.CreateToolhelp32Snapshot(windows.TH32CS_SNAPPROCESS, 0)
	if err != nil {
		return helpProcessSample{}, errors.New("could not list the system processes")
	}
	defer windows.CloseHandle(snapshot)

	alive := map[uint32]helpSystemProcess{}
	entry := windows.ProcessEntry32{Size: uint32(unsafe.Sizeof(windows.ProcessEntry32{}))}
	if err := windows.Process32First(snapshot, &entry); err != nil {
		return helpProcessSample{}, errors.New("could not read the process list")
	}
	for {
		name := windows.UTF16ToString(entry.ExeFile[:])
		if name == "" {
			name = "System"
		}
		alive[entry.ProcessID] = helpSystemProcess{
			parent: entry.ParentProcessID,
			name:   name,
		}
		err = windows.Process32Next(snapshot, &entry)
		if errors.Is(err, windows.ERROR_NO_MORE_FILES) {
			break
		}
		if err != nil {
			return helpProcessSample{}, errors.New("could not enumerate the processes")
		}
	}

	selfPID := uint32(os.Getpid())
	if _, ok := alive[selfPID]; !ok {
		return helpProcessSample{}, errors.New("this process is missing from the system list")
	}

	children := map[uint32][]uint32{}
	for pid, process := range alive {
		children[process.parent] = append(children[process.parent], pid)
	}

	tracked := s.tracked
	queue := make([]uint32, 0, len(tracked)+1)
	for pid, previous := range tracked {
		if pid == selfPID || previous.createTime == nil {
			continue
		}
		if _, ok := alive[pid]; ok {
			queue = append(queue, pid)
		}
	}
	queue = append(queue, selfPID)

	owned := map[uint32]helpProcessMetrics{}
	visited := map[uint32]bool{}
	claimedParent := map[uint32]uint32{}
	for len(queue) > 0 {
		pid := queue[len(queue)-1]
		queue = queue[:len(queue)-1]
		if visited[pid] {
			continue
		}
		visited[pid] = true
		metrics := readHelpProcessMetrics(pid)
		if pid == selfPID {
			owned[pid] = metrics
		} else {
			if metrics.createTime == nil {
				continue
			}
			if previous, ok := tracked[pid]; ok && previous.createTime != nil &&
				*previous.createTime != *metrics.createTime {
				continue
			}
			if parentPID, ok := claimedParent[pid]; ok {
				parent, found := owned[parentPID]
				if !found || parent.createTime == nil || *metrics.createTime < *parent.createTime {
					continue
				}
			}
			owned[pid] = metrics
		}
		for _, child := range children[pid] {
			if visited[child] {
				continue
			}
			claimedParent[child] = pid
			queue = append(queue, child)
		}
	}

	published := map[uint32]helpTrackedProcess{}
	for pid, metrics := range owned {
		if metrics.createTime == nil {
			continue
		}
		parent := uint32(0)
		if pid != selfPID {
			if value, ok := claimedParent[pid]; ok {
				parent = value
			} else if previous, ok := tracked[pid]; ok {
				parent = previous.parent
			}
		}
		createTime := *metrics.createTime
		published[pid] = helpTrackedProcess{parent: parent, createTime: &createTime}
	}
	s.tracked = published

	processors := float64(windows.GetActiveProcessorCount(windows.ALL_PROCESSOR_GROUPS))
	if processors <= 0 {
		processors = float64(runtime.NumCPU())
	}
	if processors <= 0 {
		processors = 1
	}
	now := time.Now()
	live := map[uint32]bool{}
	rows := make([]helpProcessRow, 0, len(owned))
	for pid, metrics := range owned {
		process, ok := alive[pid]
		if !ok {
			continue
		}
		live[pid] = true
		var cpuPercent *float64
		if baseline, ok := s.baselines[pid]; ok && metrics.createTime != nil &&
			metrics.cpu100ns != nil && baseline.createTime == *metrics.createTime {
			delta := float64(0)
			if *metrics.cpu100ns > baseline.cpu100ns {
				delta = float64(*metrics.cpu100ns - baseline.cpu100ns)
			}
			wall := now.Sub(baseline.at).Seconds()
			if wall < 0.001 {
				wall = 0.001
			}
			value := math.Max(0, math.Min(100, (delta/10_000_000)/(wall*processors)*100))
			cpuPercent = &value
		}
		if metrics.createTime != nil && metrics.cpu100ns != nil {
			s.baselines[pid] = helpCpuBaseline{
				createTime: *metrics.createTime,
				cpu100ns:   *metrics.cpu100ns,
				at:         now,
			}
		} else {
			delete(s.baselines, pid)
		}
		rows = append(rows, helpProcessRow{
			Name:         process.name,
			PID:          pid,
			CreateTime:   valueOrZero(metrics.createTime),
			CPUPercent:   cpuPercent,
			PrivateMB:    mebibytes(metrics.private),
			WorkingSetMB: mebibytes(metrics.workingSet),
			IOReadMB:     mebibytes(metrics.ioRead),
			IOWriteMB:    mebibytes(metrics.ioWrite),
		})
	}
	for pid := range s.baselines {
		if !live[pid] {
			delete(s.baselines, pid)
		}
	}

	sort.Slice(rows, func(i, j int) bool {
		leftCPU, rightCPU := -1.0, -1.0
		if rows[i].CPUPercent != nil {
			leftCPU = *rows[i].CPUPercent
		}
		if rows[j].CPUPercent != nil {
			rightCPU = *rows[j].CPUPercent
		}
		if leftCPU != rightCPU {
			return leftCPU > rightCPU
		}
		leftPrivate, rightPrivate := 0.0, 0.0
		if rows[i].PrivateMB != nil {
			leftPrivate = *rows[i].PrivateMB
		}
		if rows[j].PrivateMB != nil {
			rightPrivate = *rows[j].PrivateMB
		}
		if leftPrivate != rightPrivate {
			return leftPrivate > rightPrivate
		}
		if rows[i].Name != rows[j].Name {
			return rows[i].Name < rows[j].Name
		}
		return rows[i].PID < rows[j].PID
	})

	values := func(pick func(helpProcessRow) *float64) []*float64 {
		result := make([]*float64, len(rows))
		for i, row := range rows {
			result[i] = pick(row)
		}
		return result
	}
	return helpProcessSample{
		Rows:           rows,
		TotalCPU:       helpStrictTotal(values(func(row helpProcessRow) *float64 { return row.CPUPercent })),
		TotalPrivateMB: helpStrictTotal(values(func(row helpProcessRow) *float64 { return row.PrivateMB })),
		TotalIOReadMB:  helpStrictTotal(values(func(row helpProcessRow) *float64 { return row.IOReadMB })),
		TotalIOWriteMB: helpStrictTotal(values(func(row helpProcessRow) *float64 { return row.IOWriteMB })),
	}, nil
}

func readHelpProcessMetrics(pid uint32) helpProcessMetrics {
	handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
	if err != nil {
		return helpProcessMetrics{}
	}
	defer windows.CloseHandle(handle)

	var metrics helpProcessMetrics
	var creation, exit, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(handle, &creation, &exit, &kernel, &user); err == nil {
		createTime := filetimeValue(creation)
		cpu := filetimeValue(kernel) + filetimeValue(user)
		metrics.createTime = &createTime
		metrics.cpu100ns = &cpu
	}

	workingSet, private, ok := helpMemoryCounters(handle)
	if ok {
		metrics.workingSet = &workingSet
		metrics.private = &private
	}
	ioRead, ioWrite, ok := helpIO(handle)
	if ok {
		metrics.ioRead = &ioRead
		metrics.ioWrite = &ioWrite
	}
	return metrics
}

func helpMemoryCounters(handle windows.Handle) (uint64, uint64, bool) {
	if err := helpGetProcessMemoryInfo.Find(); err != nil {
		return 0, 0, false
	}
	var counters helpProcessMemoryCountersEx
	counters.cb = uint32(unsafe.Sizeof(counters))
	result, _, _ := helpGetProcessMemoryInfo.Call(
		uintptr(handle),
		uintptr(unsafe.Pointer(&counters)),
		uintptr(counters.cb),
	)
	if result == 0 {
		return 0, 0, false
	}
	return uint64(counters.workingSetSize), uint64(counters.privateUsage), true
}

func helpIO(handle windows.Handle) (uint64, uint64, bool) {
	if err := helpGetProcessIoCounters.Find(); err != nil {
		return 0, 0, false
	}
	var counters helpIOCounters
	result, _, _ := helpGetProcessIoCounters.Call(
		uintptr(handle),
		uintptr(unsafe.Pointer(&counters)),
	)
	if result == 0 {
		return 0, 0, false
	}
	return counters.readTransferCount, counters.writeTransferCount, true
}

func filetimeValue(value windows.Filetime) uint64 {
	return uint64(value.HighDateTime)<<32 | uint64(value.LowDateTime)
}

func mebibytes(value *uint64) *float64 {
	if value == nil {
		return nil
	}
	result := float64(*value) / 1_048_576
	return &result
}

func valueOrZero(value *uint64) uint64 {
	if value == nil {
		return 0
	}
	return *value
}
