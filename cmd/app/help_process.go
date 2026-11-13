package main

import "fmt"

type helpProcessRow struct {
	Name         string
	PID          uint32
	CreateTime   uint64
	CPUPercent   *float64
	PrivateMB    *float64
	WorkingSetMB *float64
	IOReadMB     *float64
	IOWriteMB    *float64
}

type helpProcessSample struct {
	Rows           []helpProcessRow
	TotalCPU       *float64
	TotalPrivateMB *float64
	TotalIOReadMB  *float64
	TotalIOWriteMB *float64
}

func helpFormatCPU(value *float64) string {
	if value == nil {
		return "—"
	}
	return fmt.Sprintf("%.1f%%", *value)
}

func helpFormatValue(value *float64, unit string) string {
	if value == nil {
		return "—"
	}
	return fmt.Sprintf("%.1f %s", *value, unit)
}

func helpStrictTotal(values []*float64) *float64 {
	total := 0.0
	for _, value := range values {
		if value == nil {
			return nil
		}
		total += *value
	}
	return &total
}
