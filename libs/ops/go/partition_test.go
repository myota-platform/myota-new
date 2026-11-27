package ops

import "testing"

// The expected partitions are not derived from the implementation: they were
// computed independently from the FNV-1a 64 specification. libs/ops/rust asserts
// the same table. If either language drifts, one of these tests fails — which is
// the only thing keeping two implementations honest without shared code.
func TestPartitionMatchesSpecification(t *testing.T) {
	subjects := []string{
		"entity:8f3c",
		"account:44a1",
		"activation:1b2d",
		"programme:pota",
		"",
	}

	t.Run("32 partitions", func(t *testing.T) {
		want := []int{8, 8, 20, 15, 5}
		for i, subject := range subjects {
			got, err := Partition(subject, 32)
			if err != nil {
				t.Fatalf("Partition(%q, 32): unexpected error: %v", subject, err)
			}
			if got != want[i] {
				t.Errorf("Partition(%q, 32) = %d, want %d", subject, got, want[i])
			}
		}
	})

	t.Run("7 partitions", func(t *testing.T) {
		want := []int{0, 2, 4, 2, 2}
		for i, subject := range subjects {
			got, err := Partition(subject, 7)
			if err != nil {
				t.Fatalf("Partition(%q, 7): unexpected error: %v", subject, err)
			}
			if got != want[i] {
				t.Errorf("Partition(%q, 7) = %d, want %d", subject, got, want[i])
			}
		}
	})
}

func TestPartitionIsStable(t *testing.T) {
	first, err := Partition("entity:8f3c", 32)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for i := 0; i < 100; i++ {
		again, err := Partition("entity:8f3c", 32)
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}
		if again != first {
			t.Fatalf("Partition is not stable: call %d returned %d, first call returned %d", i, again, first)
		}
	}
}

func TestPartitionStaysInRange(t *testing.T) {
	for count := 1; count <= 64; count++ {
		for _, subject := range []string{"entity:8f3c", "entity:0000", "account:44a1", "x"} {
			got, err := Partition(subject, count)
			if err != nil {
				t.Fatalf("Partition(%q, %d): unexpected error: %v", subject, count, err)
			}
			if got < 0 || got >= count {
				t.Errorf("Partition(%q, %d) = %d, outside [0, %d)", subject, count, got, count)
			}
		}
	}
}

func TestPartitionRejectsNonPositiveCount(t *testing.T) {
	for _, count := range []int{0, -1, -32} {
		if _, err := Partition("entity:8f3c", count); err == nil {
			t.Errorf("Partition with partitionCount %d returned no error, want one", count)
		}
	}
}
