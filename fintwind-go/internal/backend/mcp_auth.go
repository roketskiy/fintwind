package backend

import (
	"context"

	"fintwind-go/internal/backendstore"
)

func (s *Server) mcpAuthCommand(ctx context.Context, c backendstore.Object) (backendstore.Object, error) {
	if c.String("type") == "cancelAuthenticateMcpServer" {
		_, err := s.mcpAuth.Cancel(c.String("name"))
		return backendstore.Reply("ack", nil), err
	}
	binary, err := s.providerBinary(c.String("binary"))
	if err != nil {
		return nil, err
	}
	s.fileMu.Lock()
	settings, err := readObject(s.settingsPath)
	s.fileMu.Unlock()
	if err != nil {
		return nil, err
	}
	proxy, err := managementProxy(settings)
	if err != nil {
		return nil, err
	}
	if err := s.mcpAuth.Authenticate(ctx, binary, c.String("directory"), c.String("name"), proxy); err != nil {
		return nil, err
	}
	return backendstore.Reply("ack", nil), nil
}
