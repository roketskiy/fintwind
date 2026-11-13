package backend

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"fintwind-go/internal/backendstore"
	"fintwind-go/internal/backendworkspace"
	"fintwind-go/internal/ownedprocess"
)

func githubRepository(remote string) string {
	path := ""
	for _, prefix := range []string{"git@github.com:", "ssh://git@github.com/", "https://github.com/", "http://github.com/"} {
		if strings.HasPrefix(remote, prefix) {
			path = strings.TrimPrefix(remote, prefix)
			break
		}
	}
	path = strings.TrimSuffix(strings.TrimRight(path, "/"), ".git")
	owner, repo, ok := strings.Cut(path, "/")
	if !ok || owner == "" || repo == "" || strings.Contains(repo, "/") {
		return ""
	}
	for _, r := range path {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || strings.ContainsRune("/._-", r)) {
			return ""
		}
	}
	return path
}

// An explicit profile-local token prevents gh from consulting a system keyring.
// Only the simple hosts.yml token form is accepted; aliases/includes and user
// account selectors are not interpreted as another credential source.
func profileGithubToken(profile string) (string, error) {
	root, err := os.OpenRoot(profile)
	if err != nil {
		return "", err
	}
	defer root.Close()
	file, err := root.Open(filepath.Join("home", ".config", "gh", "hosts.yml"))
	if errors.Is(err, os.ErrNotExist) {
		return "", nil
	}
	if err != nil {
		return "", errors.New("isolated GitHub credentials unavailable")
	}
	defer file.Close()
	raw, err := io.ReadAll(io.LimitReader(file, (64<<10)+1))
	if err != nil || len(raw) > 64<<10 {
		return "", errors.New("invalid isolated GitHub credentials")
	}
	github := false
	token := ""
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimRight(line, "\r")
		if strings.TrimSpace(line) == "" || strings.HasPrefix(strings.TrimSpace(line), "#") {
			continue
		}
		if !strings.HasPrefix(line, " ") && !strings.HasPrefix(line, "\t") {
			github = strings.TrimSpace(line) == "github.com:"
			continue
		}
		if !github {
			continue
		}
		trimmed := strings.TrimLeft(line, " ")
		if len(line)-len(trimmed) != 4 || !strings.HasPrefix(trimmed, "oauth_token:") {
			continue
		}
		value := strings.TrimSpace(strings.TrimPrefix(trimmed, "oauth_token:"))
		if strings.HasPrefix(value, "\"") {
			var decoded string
			if json.Unmarshal([]byte(value), &decoded) != nil {
				return "", errors.New("invalid isolated GitHub credentials")
			}
			value = decoded
		} else if strings.HasPrefix(value, "'") {
			if !strings.HasSuffix(value, "'") || len(value) < 2 {
				return "", errors.New("invalid isolated GitHub credentials")
			}
			value = strings.ReplaceAll(value[1:len(value)-1], "''", "'")
		}
		if token != "" {
			return "", errors.New("ambiguous isolated GitHub credentials")
		}
		for _, r := range value {
			if r <= 32 || r >= 127 || r == '\'' || r == '"' || r == '\\' {
				return "", errors.New("invalid isolated GitHub credentials")
			}
		}
		token = value
	}
	return token, nil
}

func captureGithub(ctx context.Context, args []string, cwd, scratch, token string, daily bool) ([]byte, uint32, error) {
	stdout, outWrite, err := os.Pipe()
	if err != nil {
		return nil, 0, err
	}
	defer stdout.Close()
	defer outWrite.Close()
	stderr, errWrite, err := os.Pipe()
	if err != nil {
		return nil, 0, err
	}
	defer stderr.Close()
	defer errWrite.Close()
	env := ownedprocess.BuildEnv(scratch, map[string]string{"GH_TOKEN": token, "GH_HOST": "github.com", "GH_CONFIG_DIR": filepath.Join(scratch, "home"), "GH_PROMPT_DISABLED": "1", "GIT_TERMINAL_PROMPT": "0", "GH_NO_UPDATE_NOTIFIER": "1", "GH_NO_EXTENSION_UPDATE_NOTIFIER": "1", "GH_TELEMETRY": "0", "NO_COLOR": "1"})
	if daily {
		env = ownedprocess.InheritEnv(map[string]string{"GH_PROMPT_DISABLED": "1", "NO_COLOR": "1"})
	}
	child, err := ownedprocess.StartWithOutput(args[0], args[1:], scratch, cwd, env, outWrite, errWrite)
	outWrite.Close()
	errWrite.Close()
	if err != nil {
		return nil, 0, err
	}
	defer child.Close()
	type captured struct {
		out bool
		raw []byte
		err error
	}
	done := make(chan captured, 2)
	read := func(file *os.File, out bool, limit int64) {
		raw, e := io.ReadAll(io.LimitReader(file, limit+1))
		if int64(len(raw)) > limit {
			e = errors.New("GitHub output exceeds budget")
		}
		done <- captured{out, raw, e}
	}
	go read(stdout, true, 1<<20)
	go read(stderr, false, 64<<10)
	ticker := time.NewTicker(25 * time.Millisecond)
	defer ticker.Stop()
	ended, readers := false, 0
	var output []byte
	var code uint32
	for !ended || readers < 2 {
		select {
		case <-ctx.Done():
			return nil, 0, ctx.Err()
		case value := <-done:
			readers++
			if value.err != nil {
				return nil, 0, value.err
			}
			if value.out {
				output = value.raw
			}
		case <-ticker.C:
			if !ended {
				var e error
				ended, e = child.Exited(0)
				if e != nil {
					return nil, 0, e
				}
				if ended {
					code, e = child.ExitCode()
					if e != nil {
						return nil, 0, e
					}
					child.Close()
				}
			}
		}
	}
	return output, code, nil
}

func (s *Server) inspectPullRequests(ctx context.Context, op backendstore.Object) (backendstore.Object, bool, error) {
	reply := func(snapshot any) (backendstore.Object, bool, error) {
		return backendstore.Reply("workspace", backendstore.Fields(map[string]any{"result": map[string]any{"type": "pullRequests", "snapshot": snapshot}})), true, nil
	}
	unavailable := func(reason string) (backendstore.Object, bool, error) {
		return reply(map[string]string{"type": "unavailable", "reason": reason})
	}
	cwd, branch, remotes, repo, err := backendworkspace.PullRequestContext(ctx, s.workspaceForPath(op.String("cwd")), op.String("cwd"))
	if err != nil {
		return nil, true, err
	}
	if !repo {
		return unavailable("notRepository")
	}
	if branch == "" {
		return unavailable("detachedHead")
	}
	sort.SliceStable(remotes, func(i, j int) bool { return remotes[i][0] == "origin" && remotes[j][0] != "origin" })
	repository := ""
	for _, remote := range remotes {
		if value := githubRepository(remote[1]); value != "" {
			repository = value
			break
		}
	}
	if repository == "" {
		return unavailable("noGithubRemote")
	}
	binary, err := exec.LookPath("gh")
	if err != nil {
		return unavailable("cliMissing")
	}
	binary, err = filepath.Abs(binary)
	if err != nil {
		return nil, true, err
	}
	profile := filepath.Dir(s.Directory)
	token := ""
	if !s.daily {
		token, err = profileGithubToken(profile)
		if err != nil {
			return nil, true, err
		}
		if token == "" {
			return unavailable("authenticationRequired")
		}
	}
	tempRoot := filepath.Join(profile, "tmp")
	if s.daily {
		tempRoot = ""
	}
	scratch, err := os.MkdirTemp(tempRoot, "github-pr-*")
	if err != nil {
		return nil, true, err
	}
	defer os.RemoveAll(scratch)
	for _, sub := range []string{"home", "tmp"} {
		if err := os.Mkdir(filepath.Join(scratch, sub), 0o700); err != nil {
			return nil, true, err
		}
	}
	args := []string{binary, "pr", "list", "--repo", repository, "--head", branch, "--state", "all", "--limit", "8", "--json", "number,title,state,isDraft,url,headRefName,baseRefName,author,additions,deletions,changedFiles,updatedAt,comments,statusCheckRollup"}
	request, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	raw, code, err := captureGithub(request, args, cwd, scratch, token, s.daily)
	if err != nil {
		return nil, true, err
	}
	if code == 4 {
		return unavailable("authenticationRequired")
	}
	if code != 0 {
		return nil, true, errors.New("GitHub pull request query failed")
	}
	var rows []backendstore.Object
	if json.Unmarshal(raw, &rows) != nil || len(rows) > 8 || string(raw) == "null" {
		return nil, true, errors.New("invalid GitHub pull request response")
	}
	requests := []map[string]any{}
	for _, row := range rows {
		for _, field := range []string{"title", "url", "state", "headRefName", "baseRefName", "updatedAt"} {
			var value string
			if len(row[field]) == 0 || string(row[field]) == "null" || json.Unmarshal(row[field], &value) != nil {
				return nil, true, errors.New("invalid GitHub pull request text")
			}
		}
		var draft bool
		if len(row["isDraft"]) == 0 || string(row["isDraft"]) == "null" || json.Unmarshal(row["isDraft"], &draft) != nil {
			return nil, true, errors.New("invalid GitHub pull request draft status")
		}
		number, err := row.Int("number")
		if err != nil || number <= 0 || row.String("url") != fmt.Sprintf("https://github.com/%s/pull/%d", repository, number) {
			return nil, true, errors.New("invalid GitHub pull request URL")
		}
		state := strings.ToLower(row.String("state"))
		switch state {
		case "open":
			if row.Bool("isDraft") {
				state = "draft"
			}
		case "merged", "closed":
		default:
			return nil, true, errors.New("invalid GitHub pull request state")
		}
		var checks []backendstore.Object
		if raw := row["statusCheckRollup"]; len(raw) > 0 && string(raw) != "null" {
			if json.Unmarshal(raw, &checks) != nil {
				return nil, true, errors.New("invalid GitHub check response")
			}
		}
		checkState := "none"
		if len(checks) > 0 {
			checkState = "passed"
		}
		for _, check := range checks {
			if check == nil {
				return nil, true, errors.New("invalid GitHub check response")
			}
			for _, field := range []string{"state", "status", "conclusion"} {
				if raw := check[field]; len(raw) > 0 && string(raw) != "null" {
					var text string
					if json.Unmarshal(raw, &text) != nil {
						return nil, true, errors.New("invalid GitHub check response")
					}
				}
			}
			conclusion := check.String("conclusion")
			if conclusion == "" {
				conclusion = check.String("state")
			}
			switch conclusion {
			case "FAILURE", "ERROR", "TIMED_OUT", "CANCELLED", "ACTION_REQUIRED", "STARTUP_FAILURE":
				checkState = "failed"
			default:
				if checkState != "failed" && check.String("state") != "SUCCESS" && !(check.String("status") == "COMPLETED" && (conclusion == "SUCCESS" || conclusion == "NEUTRAL" || conclusion == "SKIPPED")) {
					checkState = "pending"
				}
			}
		}
		author, _ := backendstore.Decode(row["author"])
		if raw := row["author"]; len(raw) > 0 && string(raw) != "null" {
			var login string
			if author == nil || len(author["login"]) == 0 || string(author["login"]) == "null" || json.Unmarshal(author["login"], &login) != nil {
				return nil, true, errors.New("invalid GitHub author response")
			}
		}
		var comments []json.RawMessage
		if len(row["comments"]) == 0 || string(row["comments"]) == "null" || json.Unmarshal(row["comments"], &comments) != nil {
			return nil, true, errors.New("invalid GitHub comment response")
		}
		for _, comment := range comments {
			var object map[string]json.RawMessage
			if json.Unmarshal(comment, &object) != nil || object == nil {
				return nil, true, errors.New("invalid GitHub comment response")
			}
		}
		card := map[string]any{"number": number, "title": row.String("title"), "url": row.String("url"), "state": state, "headBranch": row.String("headRefName"), "baseBranch": row.String("baseRefName"), "author": author.String("login"), "updatedAt": row.String("updatedAt"), "comments": len(comments), "checks": checkState}
		for _, field := range []string{"additions", "deletions", "changedFiles"} {
			n, e := row.Int(field)
			if len(row[field]) == 0 || string(row[field]) == "null" || e != nil || n < 0 {
				return nil, true, errors.New("invalid GitHub pull request counts")
			}
			card[field] = n
		}
		requests = append(requests, card)
	}
	return reply(map[string]any{"type": "ready", "repository": repository, "branch": branch, "requests": requests})
}
