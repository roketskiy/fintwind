//go:build !windows

package backendquota

import "context"

func (l *Local) fetchClaude(context.Context, bool) Quota { return empty("unavailable") }
