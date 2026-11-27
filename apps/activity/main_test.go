package main

import "testing"

func TestDescribe(t *testing.T) {
	cases := []struct {
		name string
		args []string
		want string
	}{
		{"no arguments", nil, "activity: scaffold only; behaviour arrives in chunk P5"},
		{"version flag", []string{"--version"}, "activity (scaffold)"},
		{"unrelated argument", []string{"--verbose"}, "activity: scaffold only; behaviour arrives in chunk P5"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			if got := describe(c.args); got != c.want {
				t.Errorf("describe(%q) = %q, want %q", c.args, got, c.want)
			}
		})
	}
}
