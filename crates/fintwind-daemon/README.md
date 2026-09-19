# fintwind-daemon

`fintwind-daemon` is the standalone process that hosts Fintwind's provider sessions.
It listens on loopback only, authenticates clients with
`FINTWIND_DAEMON_TOKEN`, and
prints one JSON readiness record to stdout containing its address, protocol
version, and process ID.

```text
FINTWIND_DAEMON_TOKEN=<secret> fintwind-daemon --bind 127.0.0.1:0 [--parent-pid PID]
```

Fintwind Desktop supervises this process. Debug builds use the feature-gated
`fintwind-debug-daemon` target at `target/debug/fintwind-debug-daemon`, so rebuilding
provider code replaces only the daemon. Release distributions place the signed
`fintwind-daemon` binary beside the desktop executable.

The token is a full-control capability for a trusted Fintwind client, not a user or
workspace-scoped credential. Fintwind Desktop mints a fresh token for each daemon
launch and passes it through the child's environment. The daemon does not
terminate TLS itself, and its listener binds to loopback only.
