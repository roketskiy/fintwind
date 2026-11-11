package main

import (
	"errors"
)

// Ports state for the right panel. The Rust app owns SSH port forwarding
// locally (ssh_ports.rs keeps each PortForwardTunnel and only stops tunnels
// it holds), and the daemon protocol (v42) exposes no command that lists,
// opens, or stops a forwarded port. The Go client therefore cannot safely
// bridge Ports: offering a button that reports success without a tunnel
// would be a fabricated result. This file keeps the explicit unavailable
// state so a later render can show it honestly, and offers no forward action.
type nativePortsState struct {
	unavailable string
}

// Stable automation labels for the ports surface.
const (
	nativePortsLabel      = "native-ports"
	nativePortsErrorLabel = "native-ports-error"
)

// nativePortsUnavailable explains why the surface cannot act. It names the
// missing piece instead of a generic failure.
const nativePortsUnavailable = "Port forwarding is unavailable: the daemon protocol exposes no port command"

// nativePortsFor returns the shared ports state. Forwarding is
// environment-level, not per file or tab, so one slot is enough.
// Main thread only.
func (a *app) nativePortsFor() *nativePortsState {
	if a.native.ports.unavailable == "" {
		a.native.ports.unavailable = nativePortsUnavailable
	}
	return &a.native.ports
}

// errNativePortsUnavailable is returned by every ports action until a real
// daemon command exists.
func errNativePortsUnavailable() error {
	return errors.New(nativePortsUnavailable)
}
