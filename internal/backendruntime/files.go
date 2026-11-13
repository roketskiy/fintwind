package backendruntime

import (
	"encoding/base64"
	"errors"
	"io"
	"io/fs"
	"mime"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	"fintwind-go/internal/backendengine"
	"fintwind-go/internal/model"
)

func (a *actor) promptFiles(files []model.SubmissionFile) ([]backendengine.PromptFile, error) {
	result := make([]backendengine.PromptFile, 0, len(files))
	if len(files) > 4096 {
		return nil, errors.New("too many prompt files")
	}
	for _, file := range files {
		if !filepath.IsAbs(file.Path) {
			return nil, errors.New("absolute prompt path required")
		}
		canonical, err := filepath.EvalSymlinks(file.Path)
		if err != nil {
			return nil, err
		}
		allowed := ""
		if a.manager.daily {
			allowed = filepath.Dir(canonical)
		}
		for _, root := range []string{filepath.Join(a.manager.root, "workspace"), filepath.Join(a.manager.root, "data", "attachments"), filepath.Join(a.manager.root, "data", "blobs")} {
			rel, err := filepath.Rel(root, canonical)
			if err == nil && rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
				allowed = root
				break
			}
		}
		if allowed == "" {
			return nil, errors.New("prompt path outside isolated assets")
		}
		info, err := os.Stat(canonical)
		if err != nil {
			return nil, err
		}
		if info.IsDir() {
			return nil, errors.New("directories must be listed in prompt text, not sent as files")
		}
		if info.Size() > 20<<20 {
			return nil, errors.New("prompt file exceeds provider limit")
		}
		uri := (&url.URL{Scheme: "file", Path: "/" + filepath.ToSlash(canonical)}).String()
		contentType := mime.TypeByExtension(strings.ToLower(filepath.Ext(canonical)))
		if strings.HasPrefix(contentType, "image/") || contentType == "application/pdf" {
			root, err := os.OpenRoot(allowed)
			if err != nil {
				return nil, err
			}
			rel, _ := filepath.Rel(allowed, canonical)
			if !fs.ValidPath(filepath.ToSlash(rel)) {
				root.Close()
				return nil, errors.New("invalid prompt asset path")
			}
			opened, err := root.Open(rel)
			if err != nil {
				root.Close()
				return nil, err
			}
			bytes, err := io.ReadAll(io.LimitReader(opened, (20<<20)+1))
			opened.Close()
			root.Close()
			if err != nil || len(bytes) > 20<<20 {
				return nil, errors.New("prompt file exceeds limit")
			}
			uri = "data:" + contentType + ";base64," + base64.StdEncoding.EncodeToString(bytes)
		}
		result = append(result, backendengine.PromptFile{URI: uri, Name: file.Name})
	}
	return result, nil
}
