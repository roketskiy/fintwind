package backendworkspace

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"github.com/google/uuid"
)

type remoteRecovery struct {
	root      *os.Root
	directory string
	record    remoteRecoveryRecord
}

type remoteRecoveryRecord struct {
	Version    int    `json:"version"`
	Durability string `json:"durability"`
	Branch     string `json:"branch"`
	Before     string `json:"before"`
	Target     string `json:"target"`
	Stage      string `json:"stage"`
	CreatedAt  int64  `json:"createdAt"`
}

// Keep the exact before/after indexes and commit identities if checkout has
// started but publication cannot finish. This covers process/operation failure,
// not power loss: file Sync and rename do not guarantee durable directory entries
// on every supported filesystem. Automatic rollback could overwrite an editor's
// concurrent changes; recovery therefore requires an explicit action.
func beginRemoteRecovery(ctx context.Context, root *confinedRoot, cwd, branch, before, target string, index []byte) (*remoteRecovery, error) {
	base, err := remoteGitPath(ctx, root, cwd, "fintwind-recovery")
	if err != nil {
		return nil, err
	}
	if err := root.handle.MkdirAll(base, 0o700); err != nil {
		return nil, err
	}
	parent, err := root.handle.Open(base)
	if err != nil {
		return nil, err
	}
	entries, err := parent.ReadDir(9)
	parent.Close()
	if err != nil && err != io.EOF {
		return nil, err
	}
	if len(entries) >= 8 {
		return nil, errors.New("fast-forward recovery capacity reached; inspect retained recovery data before retrying")
	}
	directory := filepath.Join(base, uuid.NewString())
	if err := root.handle.Mkdir(directory, 0o700); err != nil {
		return nil, err
	}
	recovery := &remoteRecovery{root: root.handle, directory: directory, record: remoteRecoveryRecord{Version: 1, Durability: "process-failure", Branch: branch, Before: before, Target: target, Stage: "checkout-pending", CreatedAt: time.Now().Unix()}}
	if err := recovery.write("before-index", index); err != nil {
		recovery.remove()
		return nil, err
	}
	if err := recovery.stage("checkout-pending"); err != nil {
		recovery.remove()
		return nil, err
	}
	return recovery, nil
}

func (r *remoteRecovery) write(name string, data []byte) error {
	file, err := r.root.OpenFile(filepath.Join(r.directory, name), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	_, writeErr := file.Write(data)
	if writeErr == nil {
		writeErr = file.Sync()
	}
	closeErr := file.Close()
	if writeErr != nil {
		return writeErr
	}
	return closeErr
}

func (r *remoteRecovery) stage(stage string) error {
	r.record.Stage = stage
	raw, err := json.MarshalIndent(r.record, "", "  ")
	if err != nil {
		return err
	}
	name := "state-" + uuid.NewString() + ".tmp"
	if err := r.write(name, raw); err != nil {
		return err
	}
	if err := r.root.Rename(filepath.Join(r.directory, name), filepath.Join(r.directory, "state.json")); err != nil {
		return err
	}
	return nil
}

func (r *remoteRecovery) remove() { _ = r.root.RemoveAll(r.directory) }

func (r *remoteRecovery) failure(err error) error {
	return fmt.Errorf("%w; process-failure recovery data retained at %s (power-loss durability is not guaranteed)", err, r.directory)
}
