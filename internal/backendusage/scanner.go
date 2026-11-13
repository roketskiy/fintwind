package backendusage

import (
	"context"
	"encoding/json"
	"errors"
	"sort"
	"sync"
	"time"
)

func scan(ctx context.Context, cache *index, source string, api API, publish func(Stats)) (Stats, error) {
	rows, count, err := readListing(ctx, api)
	if err != nil {
		return Stats{}, err
	}
	seen := map[string]bool{}
	pending := []int{}
	fingerprints := make([]string, len(rows))
	now := time.Now()
	for i := range rows {
		row := &rows[i]
		seen[row.ID] = true
		encoded, err := json.Marshal(row.Facts)
		if err != nil {
			return Stats{}, err
		}
		fingerprint := string(encoded)
		fingerprints[i] = fingerprint
		recent := now.Unix() < int64(row.Facts.Updated) || now.Unix()-int64(row.Facts.Updated) < 7*86400
		raw, fresh, err := cache.split(ctx, source, row.ID, fingerprint, recent, now)
		if err != nil {
			return Stats{}, err
		}
		if len(raw) > 0 {
			var split sessionSplit
			if err := json.Unmarshal(raw, &split); err != nil {
				return Stats{}, err
			}
			row.Split = &split
		}
		if !fresh && (row.Split != nil || total(row.Facts.Tokens) > 0) {
			pending = append(pending, i)
		}
	}
	sort.SliceStable(pending, func(i, j int) bool { return rows[pending[i]].Facts.Updated > rows[pending[j]].Facts.Updated })
	progress := func(refreshing bool) Stats {
		return Stats{Entries: assemble(rows), SessionsScanned: count, Refreshing: refreshing}
	}
	stats := progress(true)
	publish(stats)
	lastPublish := time.Now()
	type result struct {
		index int
		split sessionSplit
		err   error
	}
	for start := 0; start < len(pending); start += 16 {
		if err := ctx.Err(); err != nil {
			return Stats{}, err
		}
		batch := pending[start:min(start+16, len(pending))]
		results := make(chan result, len(batch))
		var workers sync.WaitGroup
		for worker := 0; worker < min(4, len(batch)); worker++ {
			workers.Add(1)
			go func(offset int) {
				defer workers.Done()
				for j := offset; j < len(batch); j += 4 {
					i := batch[j]
					split, err := scanSplit(ctx, api, rows[i].ID)
					if err == nil && len(split.Messages) == 0 && total(rows[i].Facts.Tokens) > 0 {
						err = errors.New("usage summary has tokens but message history has no verified usage")
					}
					results <- result{index: i, split: split, err: err}
				}
			}(worker)
		}
		workers.Wait()
		close(results)
		for result := range results {
			if result.err != nil {
				continue
			}
			raw, err := json.Marshal(result.split)
			if err != nil {
				return Stats{}, err
			}
			if err := cache.saveSplit(ctx, source, rows[result.index].ID, fingerprints[result.index], raw, time.Now()); err != nil {
				return Stats{}, err
			}
			split := result.split
			rows[result.index].Split = &split
		}
		if time.Since(lastPublish) >= time.Second {
			stats = progress(true)
			if err := cache.saveSnapshot(ctx, source, stats); err != nil {
				return Stats{}, err
			}
			publish(stats)
			lastPublish = time.Now()
		}
	}
	if err := ctx.Err(); err != nil {
		return Stats{}, err
	}
	stats = progress(false)
	if err := cache.saveSnapshot(ctx, source, stats); err != nil {
		return Stats{}, err
	}
	if err := cache.retain(ctx, source, seen); err != nil {
		return Stats{}, err
	}
	return stats, nil
}
