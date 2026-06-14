package donat

import (
	"context"
	"errors"
	"testing"
)

func TestDispatchRoutesToTypedHandler(t *testing.T) {
	r := NewRegistry()
	var gotName string
	On(r, "t1_all", func(_ context.Context, ev Event[row]) error {
		gotName = ev.New.C2
		return nil
	})
	if err := r.Dispatch(context.Background(), "t1_all", []byte(insertEnvelope)); err != nil {
		t.Fatalf("Dispatch: %v", err)
	}
	if gotName != "hello" {
		t.Errorf("handler saw C2=%q, want hello", gotName)
	}
}

func TestDispatchUnknownTrigger(t *testing.T) {
	r := NewRegistry()
	err := r.Dispatch(context.Background(), "nope", []byte(insertEnvelope))
	if !errors.Is(err, ErrNoHandler) {
		t.Errorf("err = %v, want ErrNoHandler", err)
	}
}

func TestNamesListsRegistered(t *testing.T) {
	r := NewRegistry()
	On(r, "b", func(_ context.Context, _ Event[row]) error { return nil })
	On(r, "a", func(_ context.Context, _ Event[row]) error { return nil })
	names := r.Names()
	if len(names) != 2 || names[0] != "a" || names[1] != "b" {
		t.Errorf("Names() = %v, want sorted [a b]", names)
	}
}
