package backendscheduled

import (
	"encoding/json"
	"errors"
	"strings"
	"time"
	_ "time/tzdata"

	"github.com/google/uuid"
)

type Schedule struct {
	Kind         string `json:"kind"`
	IntervalSecs int64  `json:"intervalSecs,omitempty"`
	At           int64  `json:"at"`
	Weekdays     int    `json:"weekdays,omitempty"`
	Timezone     string `json:"timezone,omitempty"`
}

func (s Schedule) Validate() error {
	switch s.Kind {
	case "every":
		if s.IntervalSecs < 60 || s.IntervalSecs > 31536000 {
			return errors.New("interval must be between one minute and 365 days")
		}
	case "once":
		if s.At <= 0 {
			return errors.New("invalid one-time timestamp")
		}
	case "atTime":
		if s.At < 0 || s.At >= 1440 || s.Weekdays < 1 || s.Weekdays > 127 {
			return errors.New("invalid wall-clock schedule")
		}
		if s.Timezone != "" {
			if _, err := time.LoadLocation(s.Timezone); err != nil {
				return errors.New("unknown IANA timezone")
			}
		}
	default:
		return errors.New("invalid schedule kind")
	}
	return nil
}

func (s Schedule) Next(after int64) (int64, error) {
	if err := s.Validate(); err != nil {
		return 0, err
	}
	if after < 0 {
		return 0, errors.New("negative timestamp")
	}
	switch s.Kind {
	case "every":
		if after > int64(^uint64(0)>>1)-s.IntervalSecs {
			return 0, errors.New("timestamp overflow")
		}
		return after + s.IntervalSecs, nil
	case "once":
		if s.At <= after {
			return 0, errors.New("one-time run is in the past")
		}
		return s.At, nil
	}
	zone := time.Local
	if s.Timezone != "" {
		var err error
		zone, err = time.LoadLocation(s.Timezone)
		if err != nil {
			return 0, err
		}
	}
	local := time.Unix(after, 0).In(zone)
	y, m, d := local.Date()
	date := time.Date(y, m, d, 0, 0, 0, 0, time.UTC)
	for day := 0; day < 15; day++ {
		if s.Weekdays&(1<<int(date.Weekday())) != 0 {
			// Search real instants, not time.Date's ambiguous DST normalization.
			// The first valid wall minute >= the requested minute wins; ties use
			// the earliest instant, so the repeated hour never runs twice.
			start := date.Unix() - 26*3600
			bestMinute := 1440
			var best int64
			for stamp := start; stamp <= date.Unix()+50*3600; stamp += 60 {
				candidate := time.Unix(stamp, 0).In(zone)
				cy, cm, cd := candidate.Date()
				if cy != date.Year() || cm != date.Month() || cd != date.Day() {
					continue
				}
				minute := candidate.Hour()*60 + candidate.Minute()
				if minute >= int(s.At) && minute < bestMinute {
					bestMinute = minute
					best = stamp
				}
			}
			if bestMinute < 1440 && best > after {
				return best, nil
			}
		}
		date = date.AddDate(0, 0, 1)
	}
	return 0, errors.New("no valid scheduled day found")
}

type Draft struct {
	Title    string          `json:"title"`
	Prompt   string          `json:"prompt"`
	Enabled  bool            `json:"enabled"`
	Schedule Schedule        `json:"schedule"`
	Target   json.RawMessage `json:"target"`
}

func (d Draft) Validate() error {
	if strings.TrimSpace(d.Title) == "" || len(d.Title) > 512 || strings.TrimSpace(d.Prompt) == "" || len(d.Prompt) > 1<<20 {
		return errors.New("invalid task title or prompt")
	}
	if err := d.Schedule.Validate(); err != nil {
		return err
	}
	var target struct {
		Kind      string `json:"kind"`
		SessionID string `json:"sessionId"`
		ProjectID string `json:"projectId"`
		Launch    struct {
			Model string `json:"model"`
		} `json:"launch"`
		Workspace struct {
			Kind string `json:"kind"`
		} `json:"workspace"`
	}
	if err := json.Unmarshal(d.Target, &target); err != nil {
		return err
	}
	switch target.Kind {
	case "existingSession":
		return uuid.Validate(target.SessionID)
	case "newSession":
		if err := uuid.Validate(target.ProjectID); err != nil {
			return err
		}
		if strings.TrimSpace(target.Launch.Model) == "" || target.Workspace.Kind == "newWorktree" {
			return errors.New("scheduled task requires a model and existing workspace")
		}
	default:
		return errors.New("invalid scheduled target")
	}
	return nil
}

type Edit struct {
	Kind             string `json:"kind"`
	ID               string `json:"id"`
	ExpectedRevision int64  `json:"expectedRevision"`
	Draft            Draft  `json:"draft"`
}
type Pending struct {
	ID        string  `json:"id"`
	DueAt     int64   `json:"dueAt"`
	SessionID *string `json:"sessionId"`
	Preparing bool    `json:"preparing"`
}
type Outcome struct {
	Kind         string  `json:"outcome"`
	SessionID    *string `json:"sessionId,omitempty"`
	SubmissionID *string `json:"submissionId,omitempty"`
	Reason       string  `json:"reason,omitempty"`
}
type Record struct {
	DueAt   int64   `json:"dueAt"`
	At      int64   `json:"at"`
	Outcome Outcome `json:"outcome"`
}
type Task struct {
	Draft
	ID        string   `json:"id"`
	Revision  int64    `json:"revision"`
	NextDueAt *int64   `json:"nextDueAt"`
	CreatedAt int64    `json:"createdAt"`
	UpdatedAt int64    `json:"updatedAt"`
	Pending   *Pending `json:"pending"`
	Last      *Record  `json:"last"`
	RunCount  uint64   `json:"runCount"`
}
type Occurrence struct {
	ID         string          `json:"id"`
	TaskID     string          `json:"taskId"`
	DueAt      int64           `json:"dueAt"`
	Manual     bool            `json:"manual"`
	Owner      string          `json:"owner"`
	Draft      Draft           `json:"draft"`
	State      string          `json:"state"`
	SessionID  *string         `json:"sessionId"`
	RuntimeID  *string         `json:"runtimeId"`
	Submission json.RawMessage `json:"submission"`
}
