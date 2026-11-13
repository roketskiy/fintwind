package backendattachments

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"fintwind-go/internal/backendstore"

	"github.com/google/uuid"
)

const blobScheme = "fintwind-blob:"
const attachmentScheme = "fintwind-attachment:"
const maxFile = 20 << 20
const maxTotal = 32 << 20

type LiveReferences func(context.Context) (map[string]bool, error)
type Service struct {
	data, workspace string
	daily           bool
	live            LiveReferences
	mu              sync.Mutex
}

func New(data, workspace string, daily bool, live LiveReferences) *Service {
	return &Service{data: data, workspace: workspace, daily: daily, live: live}
}

func (s *Service) Handle(ctx context.Context, raw json.RawMessage) (json.RawMessage, bool, error) {
	command, err := backendstore.Decode(raw)
	if err != nil {
		return nil, false, err
	}
	kind := command.String("type")
	switch kind {
	case "storeBlob", "readBlob", "importAttachment", "importPathAttachment", "readAttachment", "sweepBlobs":
	default:
		return nil, false, nil
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := ctx.Err(); err != nil {
		return nil, true, err
	}
	result, err := s.dispatch(ctx, kind, command)
	if err != nil {
		return nil, true, err
	}
	encoded, err := json.Marshal(result)
	return encoded, true, err
}

func (s *Service) dispatch(ctx context.Context, kind string, c backendstore.Object) (backendstore.Object, error) {
	switch kind {
	case "storeBlob":
		bytes, err := base64.StdEncoding.DecodeString(c.String("bytes"))
		if err != nil || len(bytes) > maxFile {
			return nil, errors.New("invalid or oversized blob")
		}
		mime := strings.ToLower(c.String("mimeType"))
		extension := map[string]string{"image/png": "png", "image/jpeg": "jpg", "image/jpg": "jpg", "image/gif": "gif", "image/webp": "webp", "image/bmp": "bmp", "image/svg+xml": "svg", "image/tiff": "tiff", "image/x-icon": "ico", "image/vnd.microsoft.icon": "ico", "image/x-portable-anymap": "pnm"}[mime]
		if extension == "" {
			extension = "bin"
		}
		hash := uint64(0xcbf29ce484222325)
		for _, b := range bytes {
			hash ^= uint64(b)
			hash *= 0x100000001b3
		}
		hash ^= uint64(len(bytes)) * 0x9e3779b97f4a7c15
		name := fmt.Sprintf("%016x.%s", hash, extension)
		path := filepath.Join(s.data, "blobs", name[:2], name)
		if err := writeAtomic(path, bytes); err != nil {
			return nil, err
		}
		return backendstore.Reply("blobStored", backendstore.Fields(map[string]any{"reference": blobScheme + name, "path": path})), nil
	case "readBlob":
		name := strings.TrimPrefix(c.String("reference"), blobScheme)
		if !strings.HasPrefix(c.String("reference"), blobScheme) || !safeName(name) || len(name) < 2 {
			return nil, errors.New("invalid blob reference")
		}
		bytes, err := readBounded(filepath.Join(s.data, "blobs", name[:2], name), maxFile)
		if err != nil {
			return nil, err
		}
		return backendstore.Reply("blobData", backendstore.Fields(map[string]any{"bytes": base64.StdEncoding.EncodeToString(bytes)})), nil
	case "importAttachment", "importPathAttachment":
		return s.importAttachment(ctx, c, kind == "importPathAttachment")
	case "readAttachment":
		id := strings.TrimPrefix(c.String("reference"), attachmentScheme)
		if uuid.Validate(id) != nil || !strings.HasPrefix(c.String("reference"), attachmentScheme) {
			return nil, errors.New("invalid attachment reference")
		}
		root, err := os.OpenRoot(filepath.Join(s.data, "attachments", id))
		if err != nil {
			return nil, err
		}
		defer root.Close()
		rel, err := filepath.Rel(filepath.Join(s.data, "attachments", id), c.String("path"))
		if err != nil || !fs.ValidPath(filepath.ToSlash(rel)) {
			return nil, errors.New("attachment path outside reference")
		}
		file, err := root.Open(rel)
		if err != nil {
			return nil, err
		}
		defer file.Close()
		bytes, err := io.ReadAll(io.LimitReader(file, maxFile+1))
		if err != nil || len(bytes) > maxFile {
			return nil, errors.New("attachment exceeds limit")
		}
		return backendstore.Reply("blobData", backendstore.Fields(map[string]any{"bytes": base64.StdEncoding.EncodeToString(bytes)})), nil
	case "sweepBlobs":
		if s.live == nil {
			return nil, errors.New("live references required before sweep")
		}
		live, err := s.live(ctx)
		if err != nil {
			return nil, err
		}
		cutoff := time.Now().Add(-time.Hour)
		for _, group := range []struct{ dir, scheme string }{{"blobs", blobScheme}, {"attachments", attachmentScheme}} {
			root := filepath.Join(s.data, group.dir)
			if group.dir == "blobs" {
				err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
					if err := ctx.Err(); err != nil {
						return err
					}
					if walkErr != nil {
						return walkErr
					}
					if entry.IsDir() {
						return nil
					}
					if entry.Type()&os.ModeSymlink != 0 {
						return nil
					}
					info, err := entry.Info()
					if err != nil {
						return err
					}
					if !live[group.scheme+entry.Name()] && info.ModTime().Before(cutoff) {
						return os.Remove(path)
					}
					return nil
				})
				if err != nil && !errors.Is(err, os.ErrNotExist) {
					return nil, err
				}
			} else {
				entries, err := os.ReadDir(root)
				if errors.Is(err, os.ErrNotExist) {
					continue
				}
				if err != nil {
					return nil, err
				}
				for _, entry := range entries {
					if uuid.Validate(entry.Name()) != nil || live[group.scheme+entry.Name()] || entry.Type()&os.ModeSymlink != 0 {
						continue
					}
					info, err := entry.Info()
					if err != nil {
						continue
					}
					if info.ModTime().Before(cutoff) {
						if err := os.RemoveAll(filepath.Join(root, entry.Name())); err != nil {
							return nil, err
						}
					}
				}
			}
		}
		return backendstore.Reply("ack", nil), nil
	}
	return nil, errors.New("attachment operation missing")
}

func safeName(name string) bool {
	return name != "" && name != "." && name != ".." && filepath.Base(name) == name && !strings.ContainsAny(name, "/\\:\x00") && !strings.Contains(name, "..")
}
func readBounded(path string, limit int64) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(data)) > limit {
		return nil, errors.New("file exceeds limit")
	}
	return data, nil
}
func writeAtomic(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	file, err := os.CreateTemp(filepath.Dir(path), ".asset-*")
	if err != nil {
		return err
	}
	name := file.Name()
	defer os.Remove(name)
	if err := file.Chmod(0o600); err != nil {
		file.Close()
		return err
	}
	if _, err := file.Write(data); err != nil {
		file.Close()
		return err
	}
	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}
	if err := file.Close(); err != nil {
		return err
	}
	return os.Rename(name, path)
}

func (s *Service) importAttachment(ctx context.Context, c backendstore.Object, fromPath bool) (backendstore.Object, error) {
	name := c.String("name")
	var upload struct {
		Kind    string `json:"kind"`
		Data    string `json:"data_base64"`
		Entries []struct {
			Path string `json:"relativePath"`
			Data string `json:"dataBase64"`
		} `json:"entries"`
	}
	if !fromPath {
		if err := json.Unmarshal(c["upload"], &upload); err != nil {
			return nil, err
		}
	}
	source := ""
	if fromPath {
		var err error
		source, err = filepath.EvalSymlinks(c.String("path"))
		if err != nil {
			return nil, err
		}
		if !s.daily {
			workspace, err := filepath.EvalSymlinks(s.workspace)
			if err != nil {
				return nil, err
			}
			rel, err := filepath.Rel(workspace, source)
			if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
				return nil, errors.New("attachment source outside workspace")
			}
		}
		name = filepath.Base(source)
	}
	if !safeName(name) {
		return nil, errors.New("invalid attachment name")
	}
	id := uuid.NewString()
	base := filepath.Join(s.data, "attachments")
	if err := os.MkdirAll(base, 0o700); err != nil {
		return nil, err
	}
	staging := filepath.Join(base, "."+id+".tmp")
	if err := os.Mkdir(staging, 0o700); err != nil {
		return nil, err
	}
	defer os.RemoveAll(staging)
	target := filepath.Join(staging, name)
	count, total := 0, 0
	write := func(relative string, data []byte) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		count++
		total += len(data)
		if count > 4096 || len(data) > maxFile || total > maxTotal {
			return errors.New("attachment limits exceeded")
		}
		if relative != "" && !fs.ValidPath(filepath.ToSlash(relative)) {
			return errors.New("invalid attachment entry")
		}
		path := target
		if relative != "" {
			path = filepath.Join(target, relative)
		}
		return writeAtomic(path, data)
	}
	isDir := upload.Kind == "directory"
	if fromPath {
		info, err := os.Stat(source)
		if err != nil {
			return nil, err
		}
		isDir = info.IsDir()
		if isDir {
			if err := os.Mkdir(target, 0o700); err != nil {
				return nil, err
			}
			err = filepath.WalkDir(source, func(path string, entry fs.DirEntry, walkErr error) error {
				if walkErr != nil {
					return walkErr
				}
				if err := ctx.Err(); err != nil {
					return err
				}
				if entry.Type()&os.ModeSymlink != 0 {
					return nil
				}
				relative, err := filepath.Rel(source, path)
				if err != nil {
					return err
				}
				if relative == "." {
					return nil
				}
				if entry.IsDir() {
					return os.MkdirAll(filepath.Join(target, relative), 0o700)
				}
				if !entry.Type().IsRegular() {
					return nil
				}
				data, err := readBounded(path, maxFile)
				if err != nil {
					return err
				}
				return write(relative, data)
			})
			if err != nil {
				return nil, err
			}
		} else {
			data, err := readBounded(source, maxFile)
			if err != nil {
				return nil, err
			}
			if err := write("", data); err != nil {
				return nil, err
			}
		}
	} else if isDir {
		if len(upload.Entries) > 4096 {
			return nil, errors.New("attachment file count exceeds limit")
		}
		if err := os.Mkdir(target, 0o700); err != nil {
			return nil, err
		}
		seen := map[string]bool{}
		for _, entry := range upload.Entries {
			if seen[entry.Path] {
				return nil, errors.New("duplicate attachment entry")
			}
			seen[entry.Path] = true
			data, err := base64.StdEncoding.DecodeString(entry.Data)
			if err != nil {
				return nil, err
			}
			if err := write(entry.Path, data); err != nil {
				return nil, err
			}
		}
	} else if upload.Kind == "file" {
		data, err := base64.StdEncoding.DecodeString(upload.Data)
		if err != nil {
			return nil, err
		}
		if err := write("", data); err != nil {
			return nil, err
		}
	} else {
		return nil, errors.New("invalid attachment kind")
	}
	final := filepath.Join(base, id)
	if err := os.Rename(staging, final); err != nil {
		return nil, err
	}
	return backendstore.Reply("attachmentStored", backendstore.Fields(map[string]any{"attachment": map[string]any{"reference": attachmentScheme + id, "path": filepath.Join(final, name), "name": name, "isDir": isDir}})), nil
}
