// Command gateway is the MyOTA API gateway.
//
// This is scaffolding from chunk F1: it builds, starts and exits, so the
// toolchain and the deployment shape can be exercised before any behaviour
// exists. F3 replaces the placeholder with a working service; until then
// there is nothing here to serve traffic.
package main

import (
	"fmt"
	"os"
)

// serviceName is used in logs and in the status line. It matches the image name
// and the Kubernetes workload prefix, so a log line names the same thing that
// kubectl and the registry do.
const serviceName = "gateway"

func main() {
	if err := run(os.Args[1:], os.Stdout); err != nil {
		fmt.Fprintf(os.Stderr, "%s: %v\n", serviceName, err)
		os.Exit(1)
	}
}

// run is separated from main so it can be tested without exiting the process.
func run(args []string, out *os.File) error {
	if _, err := fmt.Fprintln(out, describe(args)); err != nil {
		return fmt.Errorf("writing status: %w", err)
	}
	return nil
}

// describe returns the one line the scaffold prints. It exists — rather than the
// text living inline in run — so the behaviour is a pure function that a table
// test can pin.
func describe(args []string) string {
	for _, arg := range args {
		if arg == "--version" {
			return serviceName + " (scaffold)"
		}
	}
	return serviceName + ": scaffold only; behaviour arrives in chunk F3"
}
