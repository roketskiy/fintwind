package backendengine

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"

	"fintwind-go/internal/backendstore"
)

type ShellPage struct {
	Row          backendstore.Object
	Output       string
	Cursor, Size uint64
	Missing      bool
}

func (c *Client) Shell(ctx context.Context, id string, cursor uint64, owner string) (ShellPage, error) {
	ctx, cancel := context.WithTimeout(ctx, 10e9)
	defer cancel()
	path := "/api/shell/" + url.PathEscape(id)
	raw, err := c.call(ctx, "GET", path+"?directory="+url.QueryEscape(c.directory), nil)
	var status *HTTPError
	if errors.As(err, &status) && status.Status == 404 {
		return ShellPage{Missing: true}, nil
	}
	if err != nil {
		return ShellPage{}, err
	}
	response, err := backendstore.Decode(raw)
	if err != nil {
		return ShellPage{}, err
	}
	row, err := backendstore.Decode(response["data"])
	if err != nil || row == nil || row.String("id") != id {
		return ShellPage{}, errors.New("invalid shell identity")
	}
	page := ShellPage{Row: row, Cursor: cursor, Size: cursor}
	metadata, _ := backendstore.Decode(row["metadata"])
	if owner == "" || metadata.String("sessionID") != owner {
		return ShellPage{}, errors.New("shell ownership does not match session")
	}
	raw, err = c.call(ctx, "GET", fmt.Sprintf("%s/output?directory=%s&cursor=%d&limit=%d", path, url.QueryEscape(c.directory), cursor, 256<<10), nil)
	if err != nil {
		return page, err
	}
	var output struct {
		Data struct {
			Output string `json:"output"`
			Cursor uint64 `json:"cursor"`
			Size   uint64 `json:"size"`
		} `json:"data"`
	}
	if json.Unmarshal(raw, &output) != nil || output.Data.Cursor < cursor || len(output.Data.Output) > 256<<10 {
		return ShellPage{}, errors.New("invalid shell output page")
	}
	page.Output = output.Data.Output
	page.Cursor = output.Data.Cursor
	page.Size = output.Data.Size
	return page, nil
}
