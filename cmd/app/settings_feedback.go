package main

import "github.com/lobehub/lobe-mygo/pkg/feedback"

type settingsFeedbackState struct {
	operations [6]settingsOperation
	pending    []feedback.ToastProps
	mcpError   string
}

func (a *app) collectSettingsFeedback() {
	state := &a.settings.feedback
	if message := a.settings.message; message != "" {
		kind := feedback.ToastError
		if message == tr("Saved") {
			kind = feedback.ToastSuccess
		}
		state.pending = append(state.pending, feedback.ToastProps{Title: message, Type: kind})
		a.settings.message = ""
	}
	backend := &a.settings.backend
	if backend.mcpStatusError != "" && backend.mcpStatusError != state.mcpError {
		state.pending = append(state.pending, feedback.ToastProps{Title: backend.mcpStatusError, Type: feedback.ToastError, Timeout: -1})
	}
	state.mcpError = backend.mcpStatusError
	operations := [6]*settingsOperation{&backend.provider, &backend.skill, &backend.mcp, &backend.sched, &backend.integration, &backend.general.operation}
	for index, operation := range operations {
		current := *operation
		previous := state.operations[index]
		if current.err != "" {
			state.pending = append(state.pending, feedback.ToastProps{Title: current.err, Type: feedback.ToastError, Timeout: -1})
			operation.err = ""
		} else if current.message != "" && current.message != previous.message {
			state.pending = append(state.pending, feedback.ToastProps{Title: current.message, Type: feedback.ToastSuccess})
		} else if index > 0 && index < 4 && previous.writing && !current.writing && current.err == "" && !current.uncertain {
			state.pending = append(state.pending, feedback.ToastProps{Title: tr("Saved"), Type: feedback.ToastSuccess})
		}
		state.operations[index] = current
	}
}
