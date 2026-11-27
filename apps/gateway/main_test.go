package main

import "testing"

func TestDescribe(t *testing.T) {
	cases := []struct {
		name string
		args []string
		want string
	}{
		{"no arguments", nil, "gateway: scaffold only; behaviour arrives in chunk F3"},
		{"version flag", []string{"--version"}, "gateway (scaffold)"},
		{"unrelated argument", []string{"--verbose"}, "gateway: scaffold only; behaviour arrives in chunk F3"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := describe(c.args); got != c.want {
				t.Errorf("describe(%q) = %q, want %q", c.args, got, c.want)
			}
		})
	}
}
