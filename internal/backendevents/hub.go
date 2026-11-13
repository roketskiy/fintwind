// Package backendevents 是 Rust core/server.rs event hub 语义的 Go 移植。
//
// 只覆盖模块级能力：per (session, runtime) 激活绑定、epoch 隔离、sequence
// 单调、replayable 事件内存 journal、有界慢消费者队列、cursor 重放。
// 不是完整协议或应用 E2E；集成由主模型完成。
//
// 失败场景对照（tools/e2e/replaycheck 逐项验证）：
// F1 过旧 cursor：journal 已淘汰 cursor 之后仍需的事件时 Subscribe 必须
// 返回 *StaleError，禁止静默漏事件后返回空订阅。
// F2 epoch 失配：cursor epoch 与 Hub epoch 不同表示 daemon 重启，cursor
// 视为 0（重放全部现存 journal），不按旧 sequence 过滤。
// F3 并发 gap/reorder：Subscribe 与 Emit 并发时，重放快照与订阅注册在
// 同一把锁内完成，live 事件排在重放之后，sequence 严格递增无间隙。
// F4 reconnect：用上次收到的最大 sequence 做 cursor，只能收到之后的新事件。
// F5 慢消费者：每订阅者有界队列，满时 hub 移除并关闭该订阅（Err() 返回
// ErrSlowConsumer），Emit 永不阻塞。
// F6 非 replayable 的实时事件只广播不入 journal。
// F7 未激活绑定：Emit 的 (session, runtime) 与 Activate 不一致时静默丢弃。
// F8 非法 payload：Emit 校验 json.RawMessage 有效性，非法返回错误且不占 sequence。
// F9 journal 上限：单 runtime 条数、全局字节数、runtime 数量三档上限。
// F10 性能：journal 用 ring buffer（淘汰 O(1)），锁内只做内存操作不做 IO。
// F11 关闭：Hub.Close 与 Subscription.Close 幂等，关闭后 Emit/Subscribe
// 返回 ErrHubClosed，pump goroutine 必退出，不死锁。
// F12 不可变性：journal 存一次性 Marshal 的字节，调用方后续修改 payload
// 不影响已入 journal 事件。
package backendevents

import (
	"bytes"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"sync"
	"sync/atomic"
)

// 默认上限。
const (
	DefaultMaxPerRuntime = 4096
	DefaultMaxTotalBytes = 64 << 20
	DefaultMaxRuntimes   = 1024
	DefaultQueueSize     = 256
)

// ErrSlowConsumer 表示订阅者消费太慢，队列溢出后被关闭。调用方应用
// 最新 cursor 重新 Subscribe 恢复（若已淘汰则会拿到 *StaleError）。
var ErrSlowConsumer = errors.New("backendevents: slow consumer overflow")

// ErrHubClosed 表示 Hub 已关闭。
var ErrHubClosed = errors.New("backendevents: hub closed")

// StaleError 表示 cursor 太旧，journal 已无法补足缺失区间。
// 调用方必须做全量同步，不得当作普通订阅继续。
type StaleError struct {
	SessionID string
	RuntimeID string
	// WantFrom 是调用方缺失区间的起始 sequence（含）。
	WantFrom uint64
	// HaveFrom 是 journal 现存最早 sequence（含）；journal 为空时为 0。
	HaveFrom uint64
}

func (e *StaleError) Error() string {
	return "backendevents: cursor too old, journal cannot fill the gap"
}

// ReplayCursor 对应 Rust ReplayCursor：epoch 隔离，sequence>cursor 才重放。
type ReplayCursor struct {
	SessionID string `json:"sessionId"`
	RuntimeID string `json:"runtimeId"`
	Epoch     string `json:"epoch"`
	Sequence  uint64 `json:"sequence"`
}

// Cursor reports the wire high-water mark, not a storage revision.
func (h *Hub) Cursor(sessionID, runtimeID string) (ReplayCursor, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed || h.active[sessionID] != runtimeID {
		return ReplayCursor{}, false
	}
	return ReplayCursor{SessionID: sessionID, RuntimeID: runtimeID, Epoch: h.epoch, Sequence: h.seq[journalKey{session: sessionID, runtime: runtimeID}]}, true
}

// Options 控制内存上限。零值字段用默认值补齐。
type Options struct {
	// MaxPerRuntime 单 runtime journal 条数上限（Rust 对齐 4096）。
	MaxPerRuntime int
	// MaxTotalBytes 全部 journal 原始帧字节上限。
	MaxTotalBytes int64
	// MaxRuntimes journal 保留的 (session, runtime) 数量上限。
	MaxRuntimes int
	// SubscriberQueue 每订阅者 live 队列长度。溢出即关闭该订阅。
	SubscriberQueue int
}

func (o Options) withDefaults() Options {
	if o.MaxPerRuntime <= 0 {
		o.MaxPerRuntime = DefaultMaxPerRuntime
	}
	if o.MaxTotalBytes <= 0 {
		o.MaxTotalBytes = DefaultMaxTotalBytes
	}
	if o.MaxRuntimes <= 0 {
		o.MaxRuntimes = DefaultMaxRuntimes
	}
	if o.SubscriberQueue <= 0 {
		o.SubscriberQueue = DefaultQueueSize
	}
	return o
}

type journalKey struct {
	session string
	runtime string
}

// entry 是 journal ring 中的一项。frame 为不可变 wire 字节。
type entry struct {
	seq   uint64
	frame []byte
}

// journal 是定容 ring buffer：淘汰 O(1)，无 slice pop O(N)。
type journal struct {
	buf   []entry
	start int
	count int
	bytes int64
	base  uint64 // buf 中最早项的 sequence（count==0 时无意义）
}

func newJournal(capacity int) *journal {
	return &journal{buf: make([]entry, capacity)}
}

func (j *journal) oldest() (uint64, bool) {
	if j.count == 0 {
		return 0, false
	}
	return j.base, true
}

// push 追加一项；满时覆盖最旧项。
func (j *journal) push(seq uint64, frame []byte) {
	if j.count < len(j.buf) {
		pos := (j.start + j.count) % len(j.buf)
		j.buf[pos] = entry{seq: seq, frame: frame}
		if j.count == 0 {
			j.base = seq
		}
		j.count++
		j.bytes += int64(len(frame))
		return
	}
	old := j.buf[j.start]
	j.bytes -= int64(len(old.frame))
	j.buf[j.start] = entry{seq: seq, frame: frame}
	j.start = (j.start + 1) % len(j.buf)
	j.base = old.seq + 1
	j.bytes += int64(len(frame))
}

// popFront 淘汰最旧一项（字节上限时用）。
func (j *journal) popFront() {
	if j.count == 0 {
		return
	}
	old := j.buf[j.start]
	j.buf[j.start] = entry{}
	j.start = (j.start + 1) % len(j.buf)
	j.count--
	j.bytes -= int64(len(old.frame))
	j.base = old.seq + 1
}

// framesAfter 返回 sequence > after 的全部帧引用（调用方持有 hub 锁）。
func (j *journal) framesAfter(after uint64) [][]byte {
	var out [][]byte
	for i := 0; i < j.count; i++ {
		pos := (j.start + i) % len(j.buf)
		if j.buf[pos].seq > after {
			out = append(out, j.buf[pos].frame)
		}
	}
	return out
}

// Hub 是内存事件中心。零值不可用，用 New 构造。
// 所有状态变更在 mu 下完成，锁内只做内存操作（Marshal/复制/入队），不做 IO。
type Hub struct {
	epoch string
	opts  Options

	mu      sync.Mutex
	closed  bool
	active  map[string]string // session -> runtime
	seq     map[journalKey]uint64
	log     map[journalKey]*journal
	lastUse map[journalKey]uint64 // LRU 时钟，用于 MaxRuntimes 淘汰
	clock   uint64
	total   int64
	subs    map[uint64]*subscriber
	deadErr map[uint64]error
	nextSub uint64
}

// New 构造 Hub。epoch 为 daemon instance UUID（重启即换）；空串时随机生成。
func New(epoch string, opts Options) *Hub {
	if epoch == "" {
		var b [16]byte
		if _, err := rand.Read(b[:]); err != nil {
			panic("backendevents: no entropy for epoch")
		}
		epoch = hex.EncodeToString(b[:])
	}
	return &Hub{
		epoch:   epoch,
		opts:    opts.withDefaults(),
		active:  make(map[string]string),
		seq:     make(map[journalKey]uint64),
		log:     make(map[journalKey]*journal),
		lastUse: make(map[journalKey]uint64),
		subs:    make(map[uint64]*subscriber),
		deadErr: make(map[uint64]error),
	}
}

// Epoch 返回 Hub 的 epoch。
func (h *Hub) Epoch() string {
	return h.epoch
}

// Activate 绑定 (session, runtime) 并清空该 session 旧 journal 与 sequence。
// 对应 Rust begin_runtime。
func (h *Hub) Activate(sessionID, runtimeID string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.active[sessionID] = runtimeID
	h.clearSession(sessionID)
}

// Deactivate 解绑 session。runtimeID 为空表示强制结束；非空且与激活值
// 不一致时不做任何事。对应 Rust end_runtime。
func (h *Hub) Deactivate(sessionID, runtimeID string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	if runtimeID != "" && h.active[sessionID] != runtimeID {
		return
	}
	delete(h.active, sessionID)
	h.clearSession(sessionID)
}

// clearSession 清理某 session 的 journal 与 sequence。调用方持有锁。
func (h *Hub) clearSession(sessionID string) {
	for k, j := range h.log {
		if k.session == sessionID {
			h.total -= j.bytes
			delete(h.log, k)
			delete(h.seq, k)
			delete(h.lastUse, k)
		}
	}
}

// wire 帧结构，对齐 Rust ServerMessage::Event 序列化形状。
type wireEvent struct {
	Type      string        `json:"type"`
	SessionID string        `json:"sessionId"`
	RuntimeID string        `json:"runtimeId"`
	Epoch     string        `json:"epoch"`
	Sequence  uint64        `json:"sequence"`
	Event     wireEventBody `json:"event"`
}

type wireEventBody struct {
	Kind    string          `json:"kind"`
	Payload json.RawMessage `json:"payload"`
}

// Emit 发布事件。replayable=false 时只广播不入 journal（实时事件
// 必须传 false）。未激活绑定时静默丢弃并返回 nil。payload 非法 JSON 时
// 返回错误且不占用 sequence。Hub 关闭后返回 ErrHubClosed。
func (h *Hub) Emit(sessionID, runtimeID, kind string, payload json.RawMessage, replayable bool) error {
	if len(bytes.TrimSpace(payload)) == 0 {
		payload = json.RawMessage(`null`)
	}
	if !json.Valid(payload) {
		return errors.New("backendevents: invalid payload JSON")
	}
	// 复制一份，调用方后续修改不影响已入 journal 事件（F12）。
	safe := append(json.RawMessage(nil), payload...)

	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return ErrHubClosed
	}
	if h.active[sessionID] != runtimeID {
		return nil
	}
	key := journalKey{session: sessionID, runtime: runtimeID}
	next := h.seq[key] + 1
	h.seq[key] = next
	// 锁内只做内存 Marshal，无 IO。同一份不可变帧共享给 journal 与订阅者。
	frame, err := json.Marshal(wireEvent{
		Type:      "event",
		SessionID: sessionID,
		RuntimeID: runtimeID,
		Epoch:     h.epoch,
		Sequence:  next,
		Event:     wireEventBody{Kind: kind, Payload: safe},
	})
	if err != nil {
		if next <= 1 {
			delete(h.seq, key)
		} else {
			h.seq[key] = next - 1
		}
		return err
	}
	if replayable {
		h.appendJournal(key, next, frame)
	}
	h.broadcast(frame)
	return nil
}

func (h *Hub) appendJournal(key journalKey, seq uint64, frame []byte) {
	j, ok := h.log[key]
	if !ok {
		if len(h.log) >= h.opts.MaxRuntimes {
			h.evictLRUJournal()
		}
		j = newJournal(h.opts.MaxPerRuntime)
		h.log[key] = j
	}
	before := j.bytes
	j.push(seq, frame)
	h.total += j.bytes - before
	h.clock++
	h.lastUse[key] = h.clock
	for h.total > h.opts.MaxTotalBytes {
		if !h.evictLargestFront() {
			break
		}
	}
}

// evictLRUJournal 淘汰最近最少使用的整个 journal。调用方持有锁。
func (h *Hub) evictLRUJournal() {
	var victim journalKey
	var victimClock uint64
	found := false
	for k, c := range h.lastUse {
		if !found || c < victimClock {
			victim, victimClock, found = k, c, true
		}
	}
	if !found {
		return
	}
	h.total -= h.log[victim].bytes
	delete(h.log, victim)
	delete(h.seq, victim)
	delete(h.lastUse, victim)
}

// evictLargestFront 从字节数最多的 journal 淘汰最旧一项。调用方持有锁。
func (h *Hub) evictLargestFront() bool {
	var victim *journal
	for _, j := range h.log {
		if j.count == 0 {
			continue
		}
		if victim == nil || j.bytes > victim.bytes {
			victim = j
		}
	}
	if victim == nil {
		return false
	}
	before := victim.bytes
	victim.popFront()
	h.total -= before - victim.bytes
	return true
}

// broadcast 把同一份不可变帧发给全部订阅者；队列满的订阅者被移除关闭。
// 调用方持有锁。
func (h *Hub) broadcast(frame []byte) {
	for id, sub := range h.subs {
		select {
		case sub.queue <- frame:
		default:
			delete(h.subs, id)
			sub.err.Store(ErrSlowConsumer)
			h.retire(id, sub)
			close(sub.queue)
			sub.stop()
		}
	}
}

type subscriber struct {
	queue    chan []byte
	out      chan []byte
	done     chan struct{}
	doneOnce sync.Once
	err      atomic.Value // error
}

func (s *subscriber) stop() {
	s.doneOnce.Do(func() { close(s.done) })
}

// Subscription 是一次订阅。Events 关闭表示订阅结束，
// 用 Err() 查询原因（nil 为正常关闭，ErrSlowConsumer 为溢出）。
type Subscription struct {
	// Events 按 sequence 递增交付原始 wire 帧；关闭即结束。
	Events chan []byte
	hub    *Hub
	id     uint64

	closeOnce sync.Once
}

// Err 返回订阅结束原因。
func (s *Subscription) Err() error {
	return s.hub.subErr(s.id)
}

// Close 幂等关闭订阅。
func (s *Subscription) Close() {
	s.closeOnce.Do(func() {
		s.hub.unsubscribe(s.id)
	})
}

func (h *Hub) subErr(id uint64) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if sub, ok := h.subs[id]; ok {
		if v := sub.err.Load(); v != nil {
			return v.(error)
		}
		return nil
	}
	return h.deadErr[id]
}

// Subscribe 按 cursors 重放后接收 live 事件。
// 同 epoch cursor 且 journal 无法补足缺失区间时返回 *StaleError 且不建订阅。
// Hub 关闭后返回 ErrHubClosed。
func (h *Hub) Subscribe(cursors []ReplayCursor) (*Subscription, error) {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return nil, ErrHubClosed
	}
	// 先校验全部 cursor：任一 stale 即整体失败，避免静默漏事件（F1）。
	type plan struct {
		key   journalKey
		after uint64
	}
	var plans []plan
	planned := make(map[journalKey]uint64)
	addPlan := func(key journalKey, after uint64) {
		if prev, ok := planned[key]; !ok || after < prev {
			planned[key] = after
		}
	}
	for _, c := range cursors {
		key := journalKey{session: c.SessionID, runtime: c.RuntimeID}
		if c.Epoch != "" && c.Epoch != h.epoch {
			// daemon 重启，旧 cursor 作废：重放该 key 全部现存 journal（F2）。
			if j, ok := h.log[key]; ok && j.count > 0 {
				addPlan(key, 0)
			}
			continue
		}
		j, ok := h.log[key]
		if !ok || j.count == 0 {
			if c.Sequence > 0 {
				h.mu.Unlock()
				return nil, &StaleError{SessionID: c.SessionID, RuntimeID: c.RuntimeID, WantFrom: c.Sequence + 1}
			}
			continue
		}
		oldest, _ := j.oldest()
		if c.Sequence+1 < oldest {
			h.mu.Unlock()
			return nil, &StaleError{SessionID: c.SessionID, RuntimeID: c.RuntimeID, WantFrom: c.Sequence + 1, HaveFrom: oldest}
		}
		addPlan(key, c.Sequence)
	}
	for key, after := range planned {
		plans = append(plans, plan{key: key, after: after})
	}
	// 无 cursor 时重放全部现存 journal（对齐 Rust unwrap_or_default 语义）。
	if len(cursors) == 0 {
		for key, j := range h.log {
			if j.count == 0 {
				continue
			}
			plans = append(plans, plan{key: key})
		}
	}
	var pending [][]byte
	for _, p := range plans {
		pending = append(pending, h.log[p.key].framesAfter(p.after)...)
	}
	sub := &subscriber{
		queue: make(chan []byte, h.opts.SubscriberQueue),
		out:   make(chan []byte, h.opts.SubscriberQueue),
		done:  make(chan struct{}),
	}
	id := h.nextSub
	h.nextSub++
	h.subs[id] = sub
	h.mu.Unlock()

	out := &Subscription{Events: sub.out, hub: h, id: id}
	go pump(pending, sub)
	return out, nil
}

// pump 先交付重放快照，再按序转发 live 事件，保证无 gap 有序（F3）。
// done 关闭或 queue 关闭即关闭 Events 并退出，不死锁（F11）。
func pump(pending [][]byte, sub *subscriber) {
	defer close(sub.out)
	for _, frame := range pending {
		select {
		case sub.out <- frame:
		case <-sub.done:
			return
		}
	}
	for {
		select {
		case <-sub.done:
			return
		case frame, ok := <-sub.queue:
			if !ok {
				return
			}
			select {
			case sub.out <- frame:
			case <-sub.done:
				return
			}
		}
	}
}

func (h *Hub) unsubscribe(id uint64) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if sub, ok := h.subs[id]; ok {
		delete(h.subs, id)
		h.retire(id, sub)
		close(sub.queue)
		sub.stop()
	}
}

// retire 记录已结束订阅的错误，供 Err() 查询。调用方持有锁。
func (h *Hub) retire(id uint64, sub *subscriber) {
	if v := sub.err.Load(); v != nil {
		h.deadErr[id] = v.(error)
		if len(h.deadErr) > 1024 {
			for k := range h.deadErr {
				delete(h.deadErr, k)
				break
			}
		}
	}
}

// Close 幂等关闭 Hub：后续 Emit/Subscribe 返回 ErrHubClosed，全部订阅结束。
func (h *Hub) Close() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.closed = true
	for id, sub := range h.subs {
		delete(h.subs, id)
		h.retire(id, sub)
		close(sub.queue)
		sub.stop()
	}
}
