// Command replaycheck 是 backendevents 的模块级 integrated E2E。
//
// 只验证 internal/backendevents（Rust core/server.rs event hub 语义移植），
// 不启动 daemon 进程，不是完整协议或应用 E2E。
// 报告写入 ownedprocess.NewRoundDir() 下的 result.json。
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendevents"
	"fintwind-go/internal/ownedprocess"
)

type frame struct {
	Type      string `json:"type"`
	SessionID string `json:"sessionId"`
	RuntimeID string `json:"runtimeId"`
	Epoch     string `json:"epoch"`
	Sequence  uint64 `json:"sequence"`
	Event     struct {
		Kind    string          `json:"kind"`
		Payload json.RawMessage `json:"payload"`
	} `json:"event"`
}

func parse(raw []byte) (frame, error) {
	var f frame
	if err := json.Unmarshal(raw, &f); err != nil {
		return f, err
	}
	return f, nil
}

func readOne(ctx context.Context, sub *backendevents.Subscription) ([]byte, error) {
	select {
	case raw, ok := <-sub.Events:
		if !ok {
			if err := sub.Err(); err != nil {
				return nil, err
			}
			return nil, errors.New("subscription closed")
		}
		return raw, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

func main() {
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	if err := run(ctx); err != nil {
		fmt.Fprintln(os.Stderr, "[X]", err)
		os.Exit(1)
	}
}

func run(ctx context.Context) (err error) {
	round, err := ownedprocess.NewRoundDir()
	if err != nil {
		return err
	}
	report := map[string]any{"checks": map[string]bool{}, "note": "module-level integrated E2E for backendevents; not full protocol/app E2E"}
	checks := report["checks"].(map[string]bool)
	defer func() {
		if err != nil {
			report["error"] = err.Error()
		}
		report["passed"] = err == nil
		data, marshalErr := json.MarshalIndent(report, "", "  ")
		if marshalErr == nil {
			marshalErr = os.WriteFile(filepath.Join(round, "result.json"), data, 0o600)
		}
		if marshalErr != nil {
			err = errors.Join(err, errors.New("could not save verification artifact"))
		}
		fmt.Println("[REPORT]", filepath.Join(round, "result.json"))
	}()
	fail := func(name string, e error) error {
		return fmt.Errorf("%s: %w", name, e)
	}

	// shape：协议形状与字段命名（camelCase，对齐 Rust ServerMessage::Event）。
	hub := backendevents.New("epoch-shape", backendevents.Options{})
	defer hub.Close()
	hub.Activate("s1", "r1")
	if e := hub.Emit("s1", "r1", "textDelta", json.RawMessage(`{"delta":"hi"}`), true); e != nil {
		return fail("shape-emit", e)
	}
	sub, e := hub.Subscribe(nil)
	if e != nil {
		return fail("shape-subscribe", e)
	}
	raw, e := readOne(ctx, sub)
	if e != nil {
		return fail("shape-read", e)
	}
	sub.Close()
	f, e := parse(raw)
	if e != nil {
		return fail("shape-parse", e)
	}
	if f.Type != "event" || f.SessionID != "s1" || f.RuntimeID != "r1" || f.Epoch != "epoch-shape" || f.Sequence != 1 || f.Event.Kind != "textDelta" || string(f.Event.Payload) != `{"delta":"hi"}` {
		return fail("shape-literal", fmt.Errorf("got %+v", f))
	}
	if strings.Contains(string(raw), "session_id") || strings.Contains(string(raw), "runtime_id") {
		return fail("shape-camel", errors.New("snake_case keys present"))
	}
	checks["shape-wire-protocol"] = true

	// F7 未激活绑定丢弃；错误 runtimeID 的 Deactivate 不动现绑定。
	hub7 := backendevents.New("epoch-f7", backendevents.Options{})
	defer hub7.Close()
	if e := hub7.Emit("s", "r", "k", json.RawMessage(`1`), true); e != nil {
		return fail("f7-emit-inactive", e)
	}
	hub7.Activate("s", "r")
	hub7.Deactivate("s", "other")
	if e := hub7.Emit("s", "wrong", "k", json.RawMessage(`1`), true); e != nil {
		return fail("f7-emit-wrong-runtime", e)
	}
	hub7.Deactivate("s", "r")
	if e := hub7.Emit("s", "r", "k", json.RawMessage(`1`), true); e != nil {
		return fail("f7-emit-after-end", e)
	}
	sub7, e := hub7.Subscribe(nil)
	if e != nil {
		return fail("f7-subscribe", e)
	}
	select {
	case raw := <-sub7.Events:
		sub7.Close()
		return fail("f7-leak", fmt.Errorf("inactive event leaked: %s", raw))
	case <-time.After(200 * time.Millisecond):
	}
	sub7.Close()
	// 正常绑定仍可用。
	hub7.Activate("s", "r")
	if e := hub7.Emit("s", "r", "k", json.RawMessage(`1`), true); e != nil {
		return fail("f7-emit-active", e)
	}
	sub7b, e := hub7.Subscribe(nil)
	if e != nil {
		return fail("f7-resubscribe", e)
	}
	if _, e := readOne(ctx, sub7b); e != nil {
		return fail("f7-read", e)
	}
	sub7b.Close()
	checks["f7-inactive-binding-dropped"] = true

	// F6 非 replayable 只广播不入 journal。
	hub6 := backendevents.New("epoch-f6", backendevents.Options{})
	defer hub6.Close()
	hub6.Activate("s", "r")
	live, e := hub6.Subscribe(nil)
	if e != nil {
		return fail("f6-subscribe-live", e)
	}
	for i := 0; i < 3; i++ {
		if e := hub6.Emit("s", "r", "liveFrame", json.RawMessage(fmt.Sprintf(`%d`, i)), false); e != nil {
			return fail("f6-emit-live", e)
		}
	}
	if e := hub6.Emit("s", "r", "textDelta", json.RawMessage(`"a"`), true); e != nil {
		return fail("f6-emit-replayable", e)
	}
	for i := 0; i < 4; i++ {
		if _, e := readOne(ctx, live); e != nil {
			return fail("f6-read-live", e)
		}
	}
	live.Close()
	late, e := hub6.Subscribe(nil)
	if e != nil {
		return fail("f6-subscribe-late", e)
	}
	raw6, e := readOne(ctx, late)
	if e != nil {
		return fail("f6-read-late", e)
	}
	f6, _ := parse(raw6)
	if f6.Event.Kind != "textDelta" || f6.Sequence != 4 {
		return fail("f6-literal", fmt.Errorf("got %+v", f6))
	}
	select {
	case raw := <-late.Events:
		late.Close()
		return fail("f6-journal-polluted", fmt.Errorf("ephemeral replayed: %s", raw))
	case <-time.After(200 * time.Millisecond):
	}
	late.Close()
	checks["f6-ephemeral-not-journalled"] = true

	// F8 非法 payload 拒绝且不占 sequence。
	hub8 := backendevents.New("epoch-f8", backendevents.Options{})
	defer hub8.Close()
	hub8.Activate("s", "r")
	if e := hub8.Emit("s", "r", "k", json.RawMessage(`{bad`), true); e == nil {
		return fail("f8-invalid-accepted", errors.New("invalid payload accepted"))
	}
	if e := hub8.Emit("s", "r", "k", json.RawMessage(`"ok"`), true); e != nil {
		return fail("f8-emit", e)
	}
	sub8, e := hub8.Subscribe(nil)
	if e != nil {
		return fail("f8-subscribe", e)
	}
	raw8, e := readOne(ctx, sub8)
	if e != nil {
		return fail("f8-read", e)
	}
	f8, _ := parse(raw8)
	if f8.Sequence != 1 {
		return fail("f8-sequence-gap", fmt.Errorf("sequence=%d want 1", f8.Sequence))
	}
	sub8.Close()
	checks["f8-invalid-payload-rejected"] = true

	// F12 已入 journal 事件不可被调用方修改。
	hub12 := backendevents.New("epoch-f12", backendevents.Options{})
	defer hub12.Close()
	hub12.Activate("s", "r")
	mutable := json.RawMessage([]byte(`{"a":1}`))
	if e := hub12.Emit("s", "r", "k", mutable, true); e != nil {
		return fail("f12-emit", e)
	}
	mutable[5] = '9'
	sub12, e := hub12.Subscribe(nil)
	if e != nil {
		return fail("f12-subscribe", e)
	}
	raw12, e := readOne(ctx, sub12)
	if e != nil {
		return fail("f12-read", e)
	}
	f12, _ := parse(raw12)
	if string(f12.Event.Payload) != `{"a":1}` {
		return fail("f12-mutated", fmt.Errorf("payload=%s", f12.Event.Payload))
	}
	sub12.Close()
	checks["f12-journal-immutable"] = true

	// F4 reconnect：cursor 续接只收新事件。
	hub4 := backendevents.New("epoch-f4", backendevents.Options{})
	defer hub4.Close()
	hub4.Activate("s", "r")
	for i := 0; i < 5; i++ {
		if e := hub4.Emit("s", "r", "k", json.RawMessage(fmt.Sprintf(`%d`, i)), true); e != nil {
			return fail("f4-emit", e)
		}
	}
	first, e := hub4.Subscribe(nil)
	if e != nil {
		return fail("f4-subscribe", e)
	}
	var last uint64
	for i := uint64(1); i <= 5; i++ {
		raw, e := readOne(ctx, first)
		if e != nil {
			return fail("f4-read", e)
		}
		ff, _ := parse(raw)
		if ff.Sequence != i {
			return fail("f4-order", fmt.Errorf("sequence=%d want %d", ff.Sequence, i))
		}
		last = ff.Sequence
	}
	first.Close()
	for i := 5; i < 8; i++ {
		if e := hub4.Emit("s", "r", "k", json.RawMessage(fmt.Sprintf(`%d`, i)), true); e != nil {
			return fail("f4-emit-more", e)
		}
	}
	re, e := hub4.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "epoch-f4", Sequence: last}})
	if e != nil {
		return fail("f4-resubscribe", e)
	}
	for i := uint64(6); i <= 8; i++ {
		raw, e := readOne(ctx, re)
		if e != nil {
			return fail("f4-reread", e)
		}
		ff, _ := parse(raw)
		if ff.Sequence != i {
			return fail("f4-reorder", fmt.Errorf("sequence=%d want %d", ff.Sequence, i))
		}
	}
	re.Close()
	checks["f4-reconnect-resume"] = true

	// F2 epoch 失配：旧 epoch cursor 重放全部，不报错。
	old, e := hub4.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "other-epoch", Sequence: 100}})
	if e != nil {
		return fail("f2-subscribe", e)
	}
	raw2, e := readOne(ctx, old)
	if e != nil {
		return fail("f2-read", e)
	}
	f2, _ := parse(raw2)
	if f2.Sequence != 1 {
		return fail("f2-literal", fmt.Errorf("sequence=%d want 1", f2.Sequence))
	}
	old.Close()
	checks["f2-epoch-mismatch-replays-all"] = true

	// F1 过旧 cursor 必须显式 error；边界 cursor（oldest-1）可重放。
	hub1 := backendevents.New("epoch-f1", backendevents.Options{MaxPerRuntime: 8})
	defer hub1.Close()
	hub1.Activate("s", "r")
	for i := 0; i < 12; i++ {
		if e := hub1.Emit("s", "r", "k", json.RawMessage(fmt.Sprintf(`%d`, i)), true); e != nil {
			return fail("f1-emit", e)
		}
	}
	if _, e := hub1.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "epoch-f1", Sequence: 3}}); e == nil {
		return fail("f1-silent-gap", errors.New("stale cursor accepted"))
	} else {
		var se *backendevents.StaleError
		if !errors.As(e, &se) || se.WantFrom != 4 || se.HaveFrom != 5 {
			return fail("f1-literal", fmt.Errorf("got %v", e))
		}
	}
	edge, e := hub1.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "epoch-f1", Sequence: 4}})
	if e != nil {
		return fail("f1-edge", e)
	}
	rawEdge, e := readOne(ctx, edge)
	if e != nil {
		return fail("f1-edge-read", e)
	}
	fe, _ := parse(rawEdge)
	if fe.Sequence != 5 {
		return fail("f1-edge-literal", fmt.Errorf("sequence=%d want 5", fe.Sequence))
	}
	edge.Close()
	if _, e := hub1.Subscribe([]backendevents.ReplayCursor{{SessionID: "ghost", RuntimeID: "r", Epoch: "epoch-f1", Sequence: 1}}); e == nil {
		return fail("f1-ghost", errors.New("cursor on evicted journal accepted"))
	}
	checks["f1-stale-cursor-errors"] = true

	// F9 字节上限与 runtime 数量上限。
	hub9 := backendevents.New("epoch-f9", backendevents.Options{MaxPerRuntime: 4096, MaxTotalBytes: 1000, MaxRuntimes: 100})
	defer hub9.Close()
	hub9.Activate("s", "r")
	big := json.RawMessage(`"` + strings.Repeat("x", 100) + `"`)
	for i := 0; i < 30; i++ {
		if e := hub9.Emit("s", "r", "k", big, true); e != nil {
			return fail("f9-emit", e)
		}
	}
	if _, e := hub9.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "epoch-f9", Sequence: 1}}); e == nil {
		return fail("f9-byte-cap", errors.New("byte cap did not evict"))
	}
	tail, e := hub9.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "epoch-f9", Sequence: 29}})
	if e != nil {
		return fail("f9-tail", e)
	}
	rawTail, e := readOne(ctx, tail)
	if e != nil {
		return fail("f9-tail-read", e)
	}
	ft, _ := parse(rawTail)
	if ft.Sequence != 30 {
		return fail("f9-tail-literal", fmt.Errorf("sequence=%d want 30", ft.Sequence))
	}
	tail.Close()
	hub9b := backendevents.New("epoch-f9b", backendevents.Options{MaxRuntimes: 2})
	defer hub9b.Close()
	for _, s := range []string{"a", "b", "c"} {
		hub9b.Activate(s, "r")
		if e := hub9b.Emit(s, "r", "k", json.RawMessage(`1`), true); e != nil {
			return fail("f9b-emit", e)
		}
	}
	if _, e := hub9b.Subscribe([]backendevents.ReplayCursor{{SessionID: "a", RuntimeID: "r", Epoch: "epoch-f9b", Sequence: 1}}); e == nil {
		return fail("f9b-runtime-cap", errors.New("runtime cap did not evict"))
	}
	if e := hub9b.Emit("a", "r", "k", json.RawMessage(`2`), true); e != nil {
		return fail("f9b-live", e)
	}
	checks["f9-memory-caps"] = true

	// F3 并发：多 goroutine emit + 中途订阅，无 gap 无 reorder。
	// 队列放大以容纳突发（慢消费者路径由 F5 覆盖）。
	hub3 := backendevents.New("epoch-f3", backendevents.Options{SubscriberQueue: 1024})
	defer hub3.Close()
	hub3.Activate("s", "r")
	const writers = 8
	const perWriter = 100
	const total3 = writers * perWriter
	var wg sync.WaitGroup
	for w := 0; w < writers; w++ {
		wg.Add(1)
		go func(w int) {
			defer wg.Done()
			for i := 0; i < perWriter; i++ {
				_ = hub3.Emit("s", "r", "k", json.RawMessage(fmt.Sprintf(`{"w":%d,"i":%d}`, w, i)), true)
			}
		}(w)
	}
	subA, e := hub3.Subscribe(nil)
	if e != nil {
		return fail("f3-subscribe-a", e)
	}
	time.Sleep(50 * time.Millisecond)
	subD, e := hub3.Subscribe(nil)
	if e != nil {
		return fail("f3-subscribe-d", e)
	}
	wg.Wait()
	checkStream := func(name string, sub *backendevents.Subscription) error {
		defer sub.Close()
		var prev uint64
		for n := 0; n < total3; n++ {
			raw, e := readOne(ctx, sub)
			if e != nil {
				return fmt.Errorf("%s stopped at %d: %w", name, n, e)
			}
			ff, e := parse(raw)
			if e != nil {
				return e
			}
			if ff.Sequence != prev+1 {
				return fmt.Errorf("%s gap: got %d want %d", name, ff.Sequence, prev+1)
			}
			prev = ff.Sequence
		}
		return nil
	}
	if e := checkStream("subA", subA); e != nil {
		return fail("f3-stream-a", e)
	}
	if e := checkStream("subD", subD); e != nil {
		return fail("f3-stream-d", e)
	}
	checks["f3-concurrent-no-gap-reorder"] = true

	// 默认上限 4096：4100 事件后 journal 保留最后 4096。
	hubCap := backendevents.New("epoch-cap", backendevents.Options{})
	defer hubCap.Close()
	hubCap.Activate("s", "r")
	start := time.Now()
	for i := 0; i < 4100; i++ {
		if e := hubCap.Emit("s", "r", "k", json.RawMessage(`1`), true); e != nil {
			return fail("f10-emit", e)
		}
	}
	if elapsed := time.Since(start); elapsed > 10*time.Second {
		return fail("f10-slow", fmt.Errorf("4100 emits took %v", elapsed))
	}
	subCap, e := hubCap.Subscribe(nil)
	if e != nil {
		return fail("f10-subscribe", e)
	}
	rawCap, e := readOne(ctx, subCap)
	if e != nil {
		return fail("f10-read", e)
	}
	fc, _ := parse(rawCap)
	if fc.Sequence != 5 {
		subCap.Close()
		return fail("f10-literal", fmt.Errorf("oldest=%d want 5", fc.Sequence))
	}
	subCap.Close()
	checks["f9f10-per-runtime-cap-4096"] = true

	// F5 慢消费者：溢出关闭供恢复，Emit 不阻塞，其他订阅者不受影响。
	hub5 := backendevents.New("epoch-f5", backendevents.Options{SubscriberQueue: 16})
	defer hub5.Close()
	hub5.Activate("s", "r")
	slow, e := hub5.Subscribe(nil)
	if e != nil {
		return fail("f5-subscribe-slow", e)
	}
	healthy, e := hub5.Subscribe(nil)
	if e != nil {
		return fail("f5-subscribe-healthy", e)
	}
	// paced rendezvous：每发一个等 healthy 收到，保证 healthy 不溢出；
	// slow 从不读，32 个之后必溢出。消除 goroutine 调度竞态。
	ack := make(chan struct{}, 200)
	raws := make([][]byte, 0, 200)
	healthyDone := make(chan error, 1)
	go func() {
		defer close(healthyDone)
		for n := 0; n < 200; n++ {
			raw, ok := <-healthy.Events
			if !ok {
				healthyDone <- fmt.Errorf("healthy closed at %d", n)
				return
			}
			raws = append(raws, raw)
			ack <- struct{}{}
		}
		healthyDone <- nil
	}()
	for i := 0; i < 200; i++ {
		if e := hub5.Emit("s", "r", "k", json.RawMessage(`1`), true); e != nil {
			return fail("f5-emit", e)
		}
		select {
		case <-ack:
		case <-ctx.Done():
			return fail("f5-healthy-stall", ctx.Err())
		}
	}
	if e := <-healthyDone; e != nil {
		return fail("f5-healthy", e)
	}
	for i, raw := range raws {
		ff, e := parse(raw)
		if e != nil || ff.Sequence != uint64(i+1) {
			return fail("f5-healthy-gap", fmt.Errorf("at %d", i))
		}
	}
	healthy.Close()
	deadline := time.Now().Add(5 * time.Second)
	for {
		select {
		case _, ok := <-slow.Events:
			if !ok {
				goto drained
			}
		case <-time.After(100 * time.Millisecond):
		}
		if time.Now().After(deadline) {
			break
		}
	}
drained:
	if !errors.Is(slow.Err(), backendevents.ErrSlowConsumer) {
		return fail("f5-overflow-error", fmt.Errorf("err=%v", slow.Err()))
	}
	slow.Close()
	// 溢出后 hub 照常服务新订阅。
	after, e := hub5.Subscribe([]backendevents.ReplayCursor{{SessionID: "s", RuntimeID: "r", Epoch: "epoch-f5", Sequence: 200}})
	if e != nil {
		return fail("f5-recover-subscribe", e)
	}
	after.Close()
	checks["f5-slow-consumer-overflow"] = true

	// F11 关闭幂等、无死锁；关闭后调用返回 ErrHubClosed。
	hub11 := backendevents.New("epoch-f11", backendevents.Options{})
	hub11.Activate("s", "r")
	sub11, e := hub11.Subscribe(nil)
	if e != nil {
		return fail("f11-subscribe", e)
	}
	var closeWg sync.WaitGroup
	for i := 0; i < 4; i++ {
		closeWg.Add(1)
		go func() {
			defer closeWg.Done()
			hub11.Close()
			sub11.Close()
		}()
	}
	emit11Done := make(chan struct{})
	go func() {
		defer close(emit11Done)
		for i := 0; i < 100; i++ {
			_ = hub11.Emit("s", "r", "k", json.RawMessage(`1`), true)
		}
	}()
	closeWg.Wait()
	select {
	case <-emit11Done:
	case <-time.After(5 * time.Second):
		return fail("f11-deadlock", errors.New("emit/close deadlocked"))
	}
	hub11.Close()
	sub11.Close()
	if e := hub11.Emit("s", "r", "k", json.RawMessage(`1`), true); !errors.Is(e, backendevents.ErrHubClosed) {
		return fail("f11-emit-closed", fmt.Errorf("err=%v", e))
	}
	if _, e := hub11.Subscribe(nil); !errors.Is(e, backendevents.ErrHubClosed) {
		return fail("f11-subscribe-closed", fmt.Errorf("err=%v", e))
	}
	if e := sub11.Err(); e != nil && !errors.Is(e, backendevents.ErrSlowConsumer) {
		return fail("f11-sub-err", e)
	}
	drainDone := make(chan struct{})
	go func() {
		defer close(drainDone)
		for range sub11.Events {
		}
	}()
	select {
	case <-drainDone:
	case <-time.After(5 * time.Second):
		return fail("f11-pump-leak", errors.New("pump did not exit"))
	}
	checks["f11-close-idempotent"] = true

	return nil
}
