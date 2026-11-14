package main

import (
	"encoding/json"

	"fintwind-go/internal/backgroundview"
	"fintwind-go/internal/model"
)

// restoreBackgroundWork seeds the capsule projection from the persisted
// background_work a hydrated session carries, so a reopened task shows its
// subagents without a live runtime. It mirrors restore_background_work
// (src/app/background_work.rs); a projection that live events already built
// is never overwritten.
func (a *app) restoreBackgroundWork(session *model.Session, raw map[string]any) {
	if session == nil || raw == nil {
		return
	}
	value, ok := raw["background_work"]
	if !ok || value == nil {
		return
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return
	}
	if backgroundview.Restore(a.extension(session), encoded) {
		a.capsule.ItemsValid = false
	}
}