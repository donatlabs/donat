package donat

import (
	"context"
	"errors"
	"sort"
	"sync"
)

// ErrNoHandler is returned by Dispatch when no handler is registered for a
// trigger name. Transports decide whether that is fatal.
var ErrNoHandler = errors.New("donat: no handler registered for trigger")

// Registry maps trigger names to typed handlers. It is transport-agnostic:
// any transport (webhook receiver, pull loop, in-process) calls Dispatch with
// the raw envelope. Handlers may be invoked concurrently and must be
// concurrent-safe.
type Registry struct {
	mu       sync.RWMutex
	handlers map[string]func(context.Context, []byte) error
}

// NewRegistry returns an empty Registry.
func NewRegistry() *Registry {
	return &Registry{handlers: make(map[string]func(context.Context, []byte) error)}
}

// On registers a typed handler for a trigger name. T is a generated row
// struct. Re-registering a name overwrites the previous handler.
func On[T any](r *Registry, triggerName string, h func(context.Context, Event[T]) error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.handlers[triggerName] = func(ctx context.Context, raw []byte) error {
		ev, err := ParseEvent[T](raw)
		if err != nil {
			return err
		}
		return h(ctx, ev)
	}
}

// Dispatch decodes and routes a raw envelope to the handler for triggerName.
// Returns ErrNoHandler if none is registered.
func (r *Registry) Dispatch(ctx context.Context, triggerName string, rawEnvelope []byte) error {
	r.mu.RLock()
	h, ok := r.handlers[triggerName]
	r.mu.RUnlock()
	if !ok {
		return ErrNoHandler
	}
	return h(ctx, rawEnvelope)
}

// Names returns the registered trigger names, sorted. A boot check can assert
// every YAML event_triggers[].name has a handler.
func (r *Registry) Names() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()
	names := make([]string, 0, len(r.handlers))
	for n := range r.handlers {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}
