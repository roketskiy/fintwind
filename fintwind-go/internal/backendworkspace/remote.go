package backendworkspace

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

type remotePlan struct{ name, fetch, push string }
type remoteTransport struct {
	directory   string
	environment map[string]string
}

func safeRemoteURL(root *confinedRoot, cwd, raw string) (string, error) {
	if raw == "" || strings.HasPrefix(raw, "-") || strings.ContainsAny(raw, "\x00\r\n") {
		return "", errors.New("invalid Git remote target")
	}
	if strings.Contains(raw, "://") {
		u, err := url.Parse(raw)
		if err != nil || (u.Scheme != "file" && u.Hostname() == "") || u.RawQuery != "" || u.Fragment != "" {
			return "", errors.New("invalid Git remote URL")
		}
		switch u.Scheme {
		case "file":
			if u.User != nil || (u.Host != "" && u.Host != "localhost") {
				return "", errors.New("non-local file remote is not allowed")
			}
			path := filepath.FromSlash(u.Path)
			if len(path) > 2 && path[0] == filepath.Separator && path[2] == ':' {
				path = path[1:]
			}
			canonical, _, e := root.resolveDir(path)
			if e != nil {
				return "", errors.New("file remote outside isolated workspace")
			}
			return canonical, nil
		case "http", "https":
			if u.User != nil {
				return "", errors.New("credentials in Git remote URLs are not allowed")
			}
		case "ssh":
			if !safeSSHHost(u.Hostname()) {
				return "", errors.New("invalid SSH remote host")
			}
			if u.User != nil {
				if !safeSSHUser(u.User.Username()) {
					return "", errors.New("invalid SSH remote user")
				}
				if _, has := u.User.Password(); has {
					return "", errors.New("password in SSH URL is not allowed")
				}
			}
		default:
			return "", errors.New("Git remote protocol is not allowed")
		}
		return raw, nil
	}
	if !filepath.IsAbs(raw) && strings.Contains(raw, ":") {
		if strings.Contains(raw, "::") {
			return "", errors.New("Git remote helpers are not allowed")
		}
		host, path, ok := strings.Cut(raw, ":")
		if !ok || host == "" || path == "" || strings.ContainsAny(host, " /\\!$`;&|<>()\t") {
			return "", errors.New("invalid SSH remote target")
		}
		if user, name, has := strings.Cut(host, "@"); has {
			if !safeSSHUser(user) || !safeSSHHost(name) {
				return "", errors.New("invalid SSH remote destination")
			}
		} else if !safeSSHHost(host) {
			return "", errors.New("invalid SSH remote host")
		}
		return raw, nil
	}
	path := raw
	if !filepath.IsAbs(path) {
		path = filepath.Join(cwd, path)
	}
	canonical, _, err := root.resolveDir(path)
	if err != nil {
		return "", errors.New("local Git remote outside isolated workspace")
	}
	return canonical, nil
}

func safeSSHUser(user string) bool {
	if user == "" || strings.HasPrefix(user, "-") {
		return false
	}
	for _, r := range user {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '_' || r == '-' || r == '.') {
			return false
		}
	}
	return true
}

func safeSSHHost(host string) bool {
	if net.ParseIP(host) != nil {
		return true
	}
	if host == "" || len(host) > 253 {
		return false
	}
	for _, label := range strings.Split(strings.TrimSuffix(host, "."), ".") {
		if label == "" || len(label) > 63 || strings.HasPrefix(label, "-") || strings.HasSuffix(label, "-") {
			return false
		}
		for _, r := range label {
			if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-') {
				return false
			}
		}
	}
	return true
}

func readRemotePlans(ctx context.Context, root *confinedRoot, cwd string) ([]remotePlan, map[string][]string, error) {
	out, err := runGit(ctx, cwd, []string{"config", "--no-includes", "--local", "--null", "--list"})
	if err != nil {
		return nil, nil, err
	}
	config := map[string][]string{}
	for _, record := range strings.Split(string(out.stdout), "\x00") {
		key, value, ok := strings.Cut(record, "\n")
		if ok {
			config[key] = append(config[key], value)
		}
	}
	if values := config["extensions.worktreeconfig"]; len(values) > 0 && values[len(values)-1] == "true" {
		return nil, nil, errors.New("remote operations with worktree-specific configuration require reconciliation")
	}
	names := []string{}
	for key := range config {
		if strings.HasPrefix(key, "remote.") && strings.HasSuffix(key, ".url") {
			names = append(names, strings.TrimSuffix(strings.TrimPrefix(key, "remote."), ".url"))
		}
	}
	sort.Strings(names)
	plans := []remotePlan{}
	for _, name := range names {
		if name == "" || strings.HasPrefix(name, "-") || strings.ContainsAny(name, "\x00\r\n /\\~^:?*[ ") {
			return nil, nil, errors.New("invalid Git remote name")
		}
		values := config["remote."+name+".url"]
		if len(values) != 1 {
			return nil, nil, errors.New("multiple remote URLs are not supported")
		}
		fetch, err := safeRemoteURL(root, cwd, values[0])
		if err != nil {
			return nil, nil, err
		}
		push := fetch
		if values := config["remote."+name+".pushurl"]; len(values) > 0 {
			if len(values) != 1 {
				return nil, nil, errors.New("multiple push URLs are not supported")
			}
			push, err = safeRemoteURL(root, cwd, values[0])
			if err != nil {
				return nil, nil, err
			}
		}
		for _, mapping := range config["remote."+name+".fetch"] {
			if mapping != "+refs/heads/*:refs/remotes/"+name+"/*" && mapping != "refs/heads/*:refs/remotes/"+name+"/*" {
				return nil, nil, errors.New("custom remote fetch mappings are not supported")
			}
		}
		plans = append(plans, remotePlan{name, fetch, push})
	}
	return plans, config, nil
}

func newRemoteTransport(ctx context.Context, root *confinedRoot, cwd string) (*remoteTransport, error) {
	objects, err := gitText(ctx, cwd, "rev-parse", "--git-path", "objects")
	if err != nil {
		return nil, err
	}
	objects = strings.TrimSpace(objects)
	if !filepath.IsAbs(objects) {
		objects = filepath.Join(cwd, objects)
	}
	objects, _, err = root.resolveDir(objects)
	if err != nil {
		return nil, errors.New("Git object store outside isolated workspace")
	}
	format, err := gitText(ctx, cwd, "rev-parse", "--show-object-format")
	if err != nil {
		return nil, err
	}
	format = strings.TrimSpace(format)
	if format != "sha1" && format != "sha256" {
		return nil, errors.New("unsupported Git object format")
	}
	scratch, err := os.MkdirTemp("", "fintwind-transport-*")
	if err != nil {
		return nil, err
	}
	failed := true
	defer func() {
		if failed {
			os.RemoveAll(scratch)
		}
	}()
	for _, path := range []string{"objects", "objects/info", "objects/pack", "refs", "refs/heads", "refs/remotes"} {
		if err := os.MkdirAll(filepath.Join(scratch, filepath.FromSlash(path)), 0o700); err != nil {
			return nil, err
		}
	}
	config := "[core]\nrepositoryformatversion = 0\nbare = true\n[credential]\nhelper =\n[protocol]\nallow = never\n[protocol \"http\"]\nallow = always\n[protocol \"https\"]\nallow = always\n[protocol \"ssh\"]\nallow = always\n[protocol \"file\"]\nallow = always\n[http]\nfollowRedirects = false\n[fetch]\nrecurseSubmodules = false\n[gc]\nauto = 0\n"
	if format == "sha256" {
		config = strings.Replace(config, "repositoryformatversion = 0", "repositoryformatversion = 1", 1) + "[extensions]\nobjectformat = sha256\n"
	}
	if err := os.WriteFile(filepath.Join(scratch, "config"), []byte(config), 0o600); err != nil {
		return nil, err
	}
	if err := os.WriteFile(filepath.Join(scratch, "HEAD"), []byte("ref: refs/heads/unused\n"), 0o600); err != nil {
		return nil, err
	}
	env := map[string]string{"GIT_DIR": scratch, "GIT_OBJECT_DIRECTORY": objects, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.DevNull, "GIT_TERMINAL_PROMPT": "0", "GIT_ASKPASS": "", "SSH_ASKPASS": "", "GIT_SSH_COMMAND": "ssh -F " + os.DevNull + " -o BatchMode=yes -o IdentityAgent=none -o IdentitiesOnly=yes -o IdentityFile=none -o UserKnownHostsFile=" + os.DevNull + " -o GlobalKnownHostsFile=" + os.DevNull + " -o StrictHostKeyChecking=yes", "GIT_SSH_VARIANT": "ssh"}
	knownHosts, err := copyProfileKnownHosts(root, scratch)
	if err != nil {
		return nil, err
	}
	shellQuote := func(value string) string { return "'" + strings.ReplaceAll(value, "'", "'\\''") + "'" }
	env["GIT_SSH_COMMAND"] = "ssh -F " + os.DevNull + " -o BatchMode=yes -o IdentityAgent=none -o IdentitiesOnly=yes -o IdentityFile=none -o GlobalKnownHostsFile=" + os.DevNull + " -o StrictHostKeyChecking=yes -o " + shellQuote(`UserKnownHostsFile="`+filepath.ToSlash(knownHosts)+`"`)
	// The proxy contains no credentials and is explicit, not inherited from a
	// repository's proxy/helper settings. Private remotes need profile credentials.
	env["HTTP_PROXY"] = "http://127.0.0.1:7897"
	env["HTTPS_PROXY"] = "http://127.0.0.1:7897"
	failed = false
	return &remoteTransport{scratch, env}, nil
}

func copyProfileKnownHosts(root *confinedRoot, scratch string) (string, error) {
	target := filepath.Join(scratch, "known_hosts")
	profile, err := os.OpenRoot(filepath.Dir(root.path))
	if err != nil {
		return "", err
	}
	defer profile.Close()
	file, err := profile.Open(filepath.Join("home", ".ssh", "known_hosts"))
	raw := []byte{}
	if err == nil {
		raw, err = io.ReadAll(io.LimitReader(file, (1<<20)+1))
		file.Close()
		if err != nil || len(raw) > 1<<20 {
			return "", errors.New("isolated SSH host trust exceeds budget")
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return "", errors.New("isolated SSH host trust unavailable")
	}
	if err := os.WriteFile(target, raw, 0o600); err != nil {
		return "", err
	}
	return target, nil
}

func (t *remoteTransport) run(ctx context.Context, cwd string, args ...string) (gitOutput, error) {
	out, err := runGitEnvironment(ctx, cwd, args, t.environment)
	if err != nil {
		if ctx.Err() != nil {
			return gitOutput{}, ctx.Err()
		}
		return gitOutput{}, errors.New("isolated Git remote operation failed")
	}
	return out, nil
}

func refValues(ctx context.Context, cwd string, env map[string]string, prefix string) (map[string]string, error) {
	out, err := runGitEnvironment(ctx, cwd, []string{"for-each-ref", "--format=%(refname) %(objectname) %(symref)", prefix}, env)
	if err != nil {
		return nil, err
	}
	refs := map[string]string{}
	for _, line := range strings.Split(string(out.stdout), "\n") {
		ref, rest, ok := strings.Cut(line, " ")
		oid, symbolic, _ := strings.Cut(rest, " ")
		// A tracking HEAD is a symbolic alias, not a fetched branch. Pruning
		// it through update-ref would dereference it and delete its target.
		if ok && symbolic == "" && strings.HasPrefix(ref, prefix) {
			refs[ref] = oid
		}
	}
	return refs, nil
}

func remoteFetch(ctx context.Context, t *remoteTransport, cwd string, plan remotePlan) error {
	prefix := "refs/remotes/" + plan.name + "/"
	old, err := refValues(ctx, cwd, nil, prefix)
	if err != nil {
		return err
	}
	var seed strings.Builder
	for ref, oid := range old {
		fmt.Fprintf(&seed, "update %s %s\n", ref, oid)
	}
	if seed.Len() > 0 {
		if _, err := runGitInput(ctx, cwd, []string{"update-ref", "--stdin"}, t.environment, []byte(seed.String())); err != nil {
			return err
		}
	}
	if _, err := t.run(ctx, cwd, "fetch", "--prune", "--no-tags", "--no-write-fetch-head", "--no-recurse-submodules", plan.fetch, "+refs/heads/*:"+prefix+"*"); err != nil {
		return err
	}
	current, err := refValues(ctx, cwd, t.environment, prefix)
	if err != nil {
		return err
	}
	var commands strings.Builder
	for ref, oid := range current {
		previous := old[ref]
		if previous == "" {
			previous = strings.Repeat("0", len(oid))
		}
		if previous != oid {
			fmt.Fprintf(&commands, "update %s %s %s\n", ref, oid, previous)
		}
	}
	for ref, oid := range old {
		if _, exists := current[ref]; !exists {
			fmt.Fprintf(&commands, "delete %s %s\n", ref, oid)
		}
	}
	if commands.Len() > 0 {
		_, err = runGitInput(ctx, cwd, []string{"update-ref", "--no-deref", "--stdin"}, nil, []byte(commands.String()))
	}
	return err
}

func remoteOperation(ctx context.Context, root *confinedRoot, cwd, kind string) error {
	if os.Getenv("FINTWIND_MYGO_PROFILE") == "daily" {
		args := []string{"push"}
		if kind == "fetchRemote" {
			args = []string{"fetch", "--all"}
		}
		if kind == "pullFastForward" {
			args = []string{"pull", "--ff-only"}
		}
		_, err := runGit(ctx, cwd, args)
		return err
	}
	top, repo, err := gitToplevel(ctx, cwd)
	if err != nil {
		return err
	}
	if !repo {
		return errors.New("workspace is not a Git repository")
	}
	cwd, _, err = root.resolveDir(top)
	if err != nil {
		return err
	}
	plans, config, err := readRemotePlans(ctx, root, cwd)
	if err != nil {
		return err
	}
	if len(plans) == 0 {
		return errors.New("no Git remote configured")
	}
	t, err := newRemoteTransport(ctx, root, cwd)
	if err != nil {
		return err
	}
	defer os.RemoveAll(t.directory)
	if kind == "fetchRemote" {
		for _, plan := range plans {
			if err := remoteFetch(ctx, t, cwd, plan); err != nil {
				return err
			}
		}
		return nil
	}
	if kind == "pullFastForward" {
		for key := range config {
			if strings.HasPrefix(key, "filter.") || key == "core.attributesfile" || key == "include.path" || strings.HasPrefix(key, "includeif.") {
				return errors.New("fast-forward with custom filters or included configuration requires reconciliation")
			}
		}
	}
	branch, err := gitText(ctx, cwd, "branch", "--show-current")
	if err != nil {
		return err
	}
	branch = strings.TrimSpace(branch)
	if branch == "" {
		return errors.New("cannot synchronize a detached HEAD")
	}
	if _, err := runGit(ctx, cwd, []string{"check-ref-format", "--branch", branch}); err != nil {
		return err
	}
	remote := ""
	merge := "refs/heads/" + branch
	if values := config["branch."+branch+".remote"]; len(values) > 0 {
		if len(values) != 1 {
			return errors.New("ambiguous branch remote")
		}
		remote = values[0]
	}
	if values := config["branch."+branch+".merge"]; len(values) > 0 {
		if len(values) != 1 {
			return errors.New("ambiguous branch upstream")
		}
		merge = values[0]
	}
	setUpstream := remote == "" || len(config["branch."+branch+".merge"]) == 0
	if setUpstream {
		if kind == "pullFastForward" {
			return errors.New("branch has no upstream")
		}
		remote = plans[0].name
		for _, plan := range plans {
			if plan.name == "origin" {
				remote = "origin"
				break
			}
		}
	}
	var selected *remotePlan
	for i := range plans {
		if plans[i].name == remote {
			selected = &plans[i]
			break
		}
	}
	if selected == nil {
		return errors.New("branch remote unavailable")
	}
	if !strings.HasPrefix(merge, "refs/heads/") {
		return errors.New("unsupported branch upstream ref")
	}
	if _, err := runGit(ctx, cwd, []string{"check-ref-format", merge}); err != nil {
		return err
	}
	head, err := gitText(ctx, cwd, "rev-parse", "--verify", "HEAD")
	if err != nil {
		return err
	}
	head = strings.TrimSpace(head)
	if kind == "push" {
		tracking := "refs/remotes/" + remote + "/" + strings.TrimPrefix(merge, "refs/heads/")
		previous, err := refValues(ctx, cwd, nil, tracking)
		if err != nil {
			return err
		}
		if _, err := t.run(ctx, cwd, "push", "--no-verify", selected.push, head+":"+merge); err != nil {
			return err
		}
		old := previous[tracking]
		if old == "" {
			old = strings.Repeat("0", len(head))
		}
		if _, err := runGit(ctx, cwd, []string{"update-ref", "--no-deref", tracking, head, old}); err != nil {
			return errors.New("push succeeded but local tracking changed; inspect before retrying")
		}
		if setUpstream {
			if _, err := runGit(ctx, cwd, []string{"config", "--local", "branch." + branch + ".remote", remote}); err != nil {
				return err
			}
			if _, err := runGit(ctx, cwd, []string{"config", "--local", "branch." + branch + ".merge", merge}); err != nil {
				return err
			}
		}
		return nil
	}
	if err := remoteFetch(ctx, t, cwd, *selected); err != nil {
		return err
	}
	target := "refs/remotes/" + remote + "/" + strings.TrimPrefix(merge, "refs/heads/")
	next, err := gitText(ctx, cwd, "rev-parse", "--verify", target)
	if err != nil {
		return err
	}
	next = strings.TrimSpace(next)
	if head == next {
		return nil
	}
	if _, err := runGit(ctx, cwd, []string{"merge-base", "--is-ancestor", head, next}); err != nil {
		return errors.New("pull is not a fast-forward")
	}
	return remoteFastForward(ctx, root, t, cwd, branch, head, next)
}
