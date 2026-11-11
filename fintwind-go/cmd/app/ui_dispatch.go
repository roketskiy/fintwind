//go:build !wiringe2e && !historye2e && !authe2e && !confige2e

package main

func dispatchHeadless(_ *app, _ func()) bool { return false }
