package main

import (
	"sort"
	"strings"

	"fintwind-go/internal/model"
)

type transcriptRowKind int

const (
	transcriptMessage transcriptRowKind = iota
	transcriptBlock
	transcriptFold
	transcriptWorking
	transcriptChangedFiles
	transcriptCompacting
)

type transcriptRow struct {
	Kind         transcriptRowKind
	MessageIndex int
	BlockIndex   int
	TurnID       string
}

// buildTranscriptRows mirrors Fintwind's folded_transcript_row_kinds: anchored
// activity blocks interleaved with messages, settled turns folded down to
// their answer, and a live working row at the tail.
func buildTranscriptRows(session *model.Session, expandedTurns map[string]bool) []transcriptRow {
	if session == nil {
		return nil
	}
	messageCount := len(session.Messages)
	blocksAfter := make([][]int, messageCount+1)
	for index := range session.TranscriptBlocks {
		anchor := session.TranscriptBlocks[index].AfterMessage
		if anchor < 0 {
			anchor = 0
		}
		if anchor > messageCount {
			anchor = messageCount
		}
		blocksAfter[anchor] = append(blocksAfter[anchor], index)
	}
	raw := make([]transcriptRow, 0, messageCount+len(session.TranscriptBlocks))
	for _, blockIndex := range blocksAfter[0] {
		raw = append(raw, transcriptRow{Kind: transcriptBlock, BlockIndex: blockIndex})
	}
	for messageIndex := 0; messageIndex < messageCount; messageIndex++ {
		raw = append(raw, transcriptRow{Kind: transcriptMessage, MessageIndex: messageIndex})
		for _, blockIndex := range blocksAfter[messageIndex+1] {
			raw = append(raw, transcriptRow{Kind: transcriptBlock, BlockIndex: blockIndex})
		}
	}

	foldAnchors := make(map[transcriptRow]string)
	hidden := make(map[transcriptRow]bool)
	for index := range session.Turns {
		turn := &session.Turns[index]
		if turn.Status == "running" {
			continue
		}
		rows := turnRows(session, turn.ID)
		answerStart := turnAnswerStart(session, rows)
		if answerStart <= 0 {
			continue
		}
		hiddenRows := rows[:answerStart]
		anchor := hiddenRows[0]
		foldAnchors[anchor] = turn.ID
		for _, row := range hiddenRows {
			hidden[row] = true
		}
	}

	rows := make([]transcriptRow, 0, len(raw)+len(foldAnchors)+1)
	for _, row := range raw {
		if turnID, ok := foldAnchors[row]; ok {
			rows = append(rows, transcriptRow{Kind: transcriptFold, TurnID: turnID})
		}
		expanded := expandedTurns[rowTurnID(session, row)]
		if expanded || !hidden[row] {
			rows = append(rows, row)
		}
	}
	if session.Busy() && session.ActiveTurnID() != "" {
		rows = append(rows, transcriptRow{Kind: transcriptWorking})
	}
	if session.Compaction != nil && session.Compaction.Status == "running" {
		rows = append(rows, transcriptRow{Kind: transcriptCompacting})
	}
	// A settled turn's file summary renders inside the terminal answer; only
	// turns without a visible answer need a standalone row, placed after the
	// fold and before the next prompt.
	changedTurns := map[string]bool{}
	for index := range session.Turns {
		turn := &session.Turns[index]
		if !turnHasChangedFiles(turn) || changedFilesInlineMessageIndex(session, turn.ID) >= 0 {
			continue
		}
		changedTurns[turn.ID] = true
	}
	if len(changedTurns) > 0 {
		lastRowByTurn := map[string]int{}
		for index, row := range rows {
			if turnID := rowTurnID(session, row); changedTurns[turnID] {
				lastRowByTurn[turnID] = index
			}
		}
		changedAfterRow := map[int]string{}
		for turnID, index := range lastRowByTurn {
			changedAfterRow[index] = turnID
		}
		spliced := make([]transcriptRow, 0, len(rows)+len(changedAfterRow))
		for index, row := range rows {
			spliced = append(spliced, row)
			if turnID, ok := changedAfterRow[index]; ok {
				spliced = append(spliced, transcriptRow{Kind: transcriptChangedFiles, TurnID: turnID})
			}
		}
		rows = spliced
	}
	return rows
}

// turnHasChangedFiles reports a settled turn whose checkpoint recorded edits.
func turnHasChangedFiles(turn *model.AgentTurn) bool {
	if turn.Status == "running" || turn.Checkpoint == nil {
		return false
	}
	return turn.Checkpoint.Status == "ready" && len(turn.Checkpoint.Files) > 0
}

// changedFilesInlineMessageIndex is the terminal answer that carries the
// turn's changed-files summary, or -1 when the summary needs its own row.
func changedFilesInlineMessageIndex(session *model.Session, turnID string) int {
	turn := session.TurnByID(turnID)
	if turn == nil || !turnHasChangedFiles(turn) {
		return -1
	}
	last := -1
	for index := range session.Messages {
		message := &session.Messages[index]
		if message.TurnID == nil || *message.TurnID != turnID {
			continue
		}
		if message.Role == "user" || message.Role == "assistant" {
			last = index
		}
	}
	if last < 0 {
		return -1
	}
	message := &session.Messages[last]
	if message.Role != "assistant" || strings.TrimSpace(message.Content) == "" ||
		message.Streaming {
		return -1
	}
	return last
}

// turnRows is the turn's own messages and anchored blocks in transcript
// order. The opening user prompt stays outside the fold as its heading.
func turnRows(session *model.Session, turnID string) []transcriptRow {
	messageCount := len(session.Messages)
	type anchored struct {
		anchor int
		index  int
	}
	blocks := make([]anchored, 0, len(session.TranscriptBlocks))
	for index := range session.TranscriptBlocks {
		block := &session.TranscriptBlocks[index]
		if block.TurnID == nil || *block.TurnID != turnID {
			continue
		}
		anchor := block.AfterMessage
		if anchor < 0 {
			anchor = 0
		}
		if anchor > messageCount {
			anchor = messageCount
		}
		blocks = append(blocks, anchored{anchor: anchor, index: index})
	}
	sort.SliceStable(blocks, func(i, j int) bool { return blocks[i].anchor < blocks[j].anchor })

	rows := make([]transcriptRow, 0, messageCount)
	next := 0
	promptSeen := false
	for messageIndex := 0; messageIndex < messageCount; messageIndex++ {
		for next < len(blocks) && blocks[next].anchor <= messageIndex {
			rows = append(rows, transcriptRow{Kind: transcriptBlock, BlockIndex: blocks[next].index})
			next++
		}
		message := &session.Messages[messageIndex]
		if message.TurnID == nil || *message.TurnID != turnID {
			continue
		}
		switch message.Role {
		case "user":
			if !promptSeen {
				promptSeen = true
				continue
			}
			rows = append(rows, transcriptRow{Kind: transcriptMessage, MessageIndex: messageIndex})
		case "assistant":
			rows = append(rows, transcriptRow{Kind: transcriptMessage, MessageIndex: messageIndex})
		}
	}
	for ; next < len(blocks); next++ {
		rows = append(rows, transcriptRow{Kind: transcriptBlock, BlockIndex: blocks[next].index})
	}
	return rows
}

// turnAnswerStart is where the turn's trailing run of assistant answers
// begins; everything before it is work that folds away.
func turnAnswerStart(session *model.Session, rows []transcriptRow) int {
	isAnswer := func(row transcriptRow) bool {
		if row.Kind != transcriptMessage {
			return false
		}
		if row.MessageIndex < 0 || row.MessageIndex >= len(session.Messages) {
			return false
		}
		message := &session.Messages[row.MessageIndex]
		return message.Role == "assistant" && strings.TrimSpace(message.Content) != ""
	}
	lastText := -1
	for index := len(rows) - 1; index >= 0; index-- {
		if isAnswer(rows[index]) {
			lastText = index
			break
		}
	}
	if lastText < 0 {
		return len(rows)
	}
	for _, row := range rows[lastText+1:] {
		if row.Kind == transcriptMessage {
			if message := &session.Messages[row.MessageIndex]; message.Role == "user" {
				return len(rows)
			}
		}
	}
	for index := lastText - 1; index >= 0; index-- {
		if !isAnswer(rows[index]) {
			return index + 1
		}
	}
	return 0
}

func rowTurnID(session *model.Session, row transcriptRow) string {
	switch row.Kind {
	case transcriptMessage:
		if row.MessageIndex >= 0 && row.MessageIndex < len(session.Messages) {
			if turnID := session.Messages[row.MessageIndex].TurnID; turnID != nil {
				return *turnID
			}
		}
	case transcriptBlock:
		if row.BlockIndex >= 0 && row.BlockIndex < len(session.TranscriptBlocks) {
			if turnID := session.TranscriptBlocks[row.BlockIndex].TurnID; turnID != nil {
				return *turnID
			}
		}
	}
	return ""
}

// messageStartsFollowupTurn reports a user prompt that opens a later turn.
func messageStartsFollowupTurn(messages []model.Message, messageIndex int) bool {
	if messageIndex < 0 || messageIndex >= len(messages) {
		return false
	}
	if messages[messageIndex].Role != "user" {
		return false
	}
	for index := 0; index < messageIndex; index++ {
		if messages[index].Role == "user" {
			return true
		}
	}
	return false
}

// rebuildTranscript refolds the row list after a disclosure change.
func (a *app) rebuildTranscript() {
	if a.applyingEvents {
		a.transcriptDirty = true
		return
	}
	if a.expandedTurns == nil {
		a.expandedTurns = map[string]bool{}
	}
	a.transcriptRows = buildTranscriptRows(a.detail, a.expandedTurns)
	a.refreshContextSummary()
}
