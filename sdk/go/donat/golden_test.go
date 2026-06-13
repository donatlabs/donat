package donat_test

import (
	"context"
	"testing"

	"github.com/donat/donat-go/donat"
	"github.com/donat/donat-go/internal/golden"
)

// Proves a generated struct composes with the SDK generics end to end.
func TestGeneratedStructComposesWithSDK(t *testing.T) {
	r := donat.NewRegistry()
	donat.On(r, "on_t1", func(_ context.Context, ev donat.Event[golden.TestT1]) error {
		_ = ev.New // type-checks: Event[golden.TestT1]
		return nil
	})
	if got := r.Names(); len(got) != 1 || got[0] != "on_t1" {
		t.Errorf("Names() = %v, want [on_t1]", got)
	}
}
