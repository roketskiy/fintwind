//go:build !windows

package main

import "errors"

type helpProcessSampler struct{}

func newHelpProcessSampler() *helpProcessSampler {
	return &helpProcessSampler{}
}

func (*helpProcessSampler) sample() (helpProcessSample, error) {
	return helpProcessSample{}, errors.New("resource sampling is only available on Windows")
}
