package backendruntime

import (
	"encoding/json"
	"os"
	"path/filepath"

	"fintwind-go/internal/backendstore"
)

// 上下文成分面板的数据源:OpenCode 服务端的 fintwind.context 插件在每次主模型
// 请求组装后写出快照文件,这里读取并换算成 app 的 v2 成分快照。分类口径见
// docs/context-composition.md;分类拆解的思路参考 dsh-context(Apache-2.0,
// 许可声明见 docs/licenses)。预估值不按供应商用量缩放。
const compositionSnapshotVersion = 1

type contextCompositionSnapshot struct {
	Version         uint32 `json:"version"`
	Model           string `json:"model"`
	System          uint64 `json:"system"`
	Tools           uint64 `json:"tools"`
	Messages        uint64 `json:"messages"`
	Reasoning       uint64 `json:"reasoning"`
	ToolCalls       uint64 `json:"toolCalls"`
	Files           uint64 `json:"files"`
	ToolResults     uint64 `json:"toolResults"`
	Memory          uint64 `json:"memory"`
	OpaqueReasoning bool   `json:"opaqueReasoning"`
	OpaqueMemory    bool   `json:"opaqueMemory"`
}

// compositionWire converts the plugin snapshot into the app's v2 composition
// wire. Incomplete marks sessions whose opaque provider states leave parts of
// the context unmeasured.
func (a *actor) compositionWire(snapshot *contextCompositionSnapshot) map[string]any {
	return map[string]any{
		"version":         2,
		"model":           snapshot.Model,
		"system":          snapshot.System,
		"tools":           snapshot.Tools,
		"memory":          snapshot.Memory,
		"messages":        snapshot.Messages,
		"reasoning":       snapshot.Reasoning,
		"toolCalls":       snapshot.ToolCalls,
		"files":           snapshot.Files,
		"toolResults":     snapshot.ToolResults,
		"opaqueReasoning": snapshot.OpaqueReasoning,
		"opaqueMemory":    snapshot.OpaqueMemory,
		"incomplete":      snapshot.OpaqueReasoning || snapshot.OpaqueMemory,
	}
}

// contextCompositionSnapshot reads the latest assembled request written by the
// plugin for this session's native identity. Missing or malformed snapshots
// yield nil: the panel then simply reports nothing yet.
func (a *actor) contextCompositionSnapshot() *contextCompositionSnapshot {
	if a.nativeID == "" || a.manager == nil {
		return nil
	}
	raw, err := os.ReadFile(filepath.Join(a.manager.root, "data", "context", a.nativeID+".json"))
	if err != nil {
		return nil
	}
	var snapshot contextCompositionSnapshot
	if json.Unmarshal(raw, &snapshot) != nil || snapshot.Version != compositionSnapshotVersion {
		return nil
	}
	return &snapshot
}

// storeComposition persists the composition inside the session snapshot so
// hydration keeps it across app restarts.
func (a *actor) storeComposition(wire map[string]any) {
	usage, err := backendstore.Decode(a.snapshot["context_usage"])
	if err != nil {
		usage = backendstore.Object{}
	}
	usage.Set("composition", wire)
	a.snapshot.Set("context_usage", usage)
	a.dirty = true
}

// refreshComposition reports the latest assembled request's composition to the
// app and persists it; without a usable snapshot it is a no-op.
func (a *actor) refreshComposition() error {
	if a.snapshot == nil {
		return nil
	}
	snapshot := a.contextCompositionSnapshot()
	if snapshot == nil {
		return nil
	}
	wire := a.compositionWire(snapshot)
	a.storeComposition(wire)
	return a.emit("contextCompositionUpdated", wire)
}
