//go:build wiringe2e || historye2e || authe2e || confige2e

package main

import "sync"

var headlessDispatchers sync.Map

func dispatchHeadless(a *app, update func()) bool {
	value, exists := headlessDispatchers.Load(a)
	if !exists {
		return false
	}
	queue := value.(chan func())
	select {
	case queue <- func() {
		select {
		case <-a.done:
			return
		default:
		}
		update()
	}:
	case <-a.done:
	case <-a.lifetime.Done():
	}
	return true
}

func installHeadlessDispatcher(a *app) (chan func(), func()) {
	queue := make(chan func(), 256)
	headlessDispatchers.Store(a, queue)
	return queue, func() { headlessDispatchers.Delete(a) }
}
