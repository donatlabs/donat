package donat

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"sync"
)

// Functions holds the Go implementations of actions declared in the metadata.
//
// An action is a custom GraphQL field the engine does not resolve from SQL —
// the answer to "this cannot be written in YAML". Declaring one without a
// `handler` says it is resolved in this process; the function registered under
// its name is what resolves it.
type Functions struct {
	mu sync.RWMutex
	fn map[string]func(context.Context, json.RawMessage) (any, error)
}

// NewFunctions returns an empty set.
func NewFunctions() *Functions {
	return &Functions{fn: make(map[string]func(context.Context, json.RawMessage) (any, error))}
}

// WithFunction registers fn as the implementation of the action named name.
//
// In and Out are ordinary structs: In is decoded from the action's arguments
// by their declared names, and Out is encoded and then shaped by the engine
// against the action's `output_type` and the caller's selection set. Nothing
// here trusts Out — a field the metadata declares non-null and the function
// leaves empty is refused, so one declaration cannot mean two different things
// depending on which host is serving.
//
//	donat.WithFunction("render_invoice_pdf",
//	    func(ctx context.Context, a Args) (Out, error) { ... })
func WithFunction[In, Out any](name string, fn func(context.Context, In) (Out, error)) Option {
	return func(c *Config) {
		if c.Functions == nil {
			c.Functions = NewFunctions()
		}
		c.Functions.register(name, func(ctx context.Context, raw json.RawMessage) (any, error) {
			var in In
			// An action with no arguments sends `{}`; decoding that into a
			// struct with no fields is fine, and into one with fields leaves
			// them zero, which is what the declaration asked for.
			if len(raw) > 0 {
				if err := json.Unmarshal(raw, &in); err != nil {
					return nil, fmt.Errorf("action %q: decoding arguments: %w", name, err)
				}
			}
			return fn(ctx, in)
		})
	}
}

func (f *Functions) register(name string, fn func(context.Context, json.RawMessage) (any, error)) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.fn[name] = fn
}

// Call invokes the function registered for name. The bool reports whether one
// was registered at all, which the caller distinguishes from the function
// itself failing.
func (f *Functions) Call(ctx context.Context, name string, input json.RawMessage) (any, bool, error) {
	f.mu.RLock()
	fn, ok := f.fn[name]
	f.mu.RUnlock()
	if !ok {
		return nil, false, nil
	}
	out, err := fn(ctx, input)
	return out, true, err
}

// Names returns the registered action names, sorted.
func (f *Functions) Names() []string {
	f.mu.RLock()
	defer f.mu.RUnlock()
	names := make([]string, 0, len(f.fn))
	for n := range f.fn {
		names = append(names, n)
	}
	sort.Strings(names)
	return names
}

// missingFunctions returns the handler-less actions in the metadata snapshot
// that nobody registered a function for.
//
// Such an action is in the schema and can never be resolved, so a host that
// started anyway would serve a field that always fails. The standalone server
// refuses the mirror image of this — a handler-less action it cannot call at
// all — and neither host may accept the declaration quietly.
func missingFunctions(actions []snapshotAction, f *Functions) []string {
	registered := make(map[string]struct{})
	if f != nil {
		for _, n := range f.Names() {
			registered[n] = struct{}{}
		}
	}
	var missing []string
	for _, a := range actions {
		if a.Definition.Handler != nil {
			continue // a webhook: not this host's to implement
		}
		if _, ok := registered[a.Name]; !ok {
			missing = append(missing, a.Name)
		}
	}
	sort.Strings(missing)
	return missing
}

// snapshotAction is the slice of an action declaration this host needs to
// check its registry against. The core owns everything else about it.
type snapshotAction struct {
	Name       string `json:"name"`
	Definition struct {
		Handler *string `json:"handler"`
	} `json:"definition"`
}

// checkFunctionsCoverActions refuses a snapshot declaring an in-process action
// that nobody implemented.
//
// The declaration and the implementation live in different files and are
// checked by different people, so they drift. Starting anyway would put a field
// in the schema that always fails, which an operator discovers from a user
// report rather than from a boot log — and `donat-server` refuses the mirror
// image of this, an action it has no handler to call.
func checkFunctionsCoverActions(snapshot []byte, f *Functions) error {
	if len(snapshot) == 0 {
		return nil
	}
	var cfg struct {
		Metadata struct {
			Actions []snapshotAction `json:"actions"`
		} `json:"metadata"`
	}
	if err := json.Unmarshal(snapshot, &cfg); err != nil {
		// A snapshot this host cannot read is core_init's to report, with the
		// detail this function does not have.
		return nil //nolint:nilerr
	}
	missing := missingFunctions(cfg.Metadata.Actions, f)
	if len(missing) == 0 {
		return nil
	}
	return fmt.Errorf(
		"donat.New: actions %v are declared without a handler, which means they are "+
			"resolved in this process, but no function is registered for them — add "+
			"donat.WithFunction(<name>, ...) for each, or give them a handler in the metadata",
		missing,
	)
}

// snapshotDeclaresAttachments reports whether any table in the snapshot
// declares a file column, which is what makes a plan unique to its request and
// therefore uncacheable.
func snapshotDeclaresAttachments(snapshot []byte) (bool, error) {
	if len(snapshot) == 0 {
		return false, nil
	}
	var cfg struct {
		Metadata struct {
			Sources []struct {
				Tables []struct {
					Attachments []json.RawMessage `json:"attachments"`
				} `json:"tables"`
			} `json:"sources"`
		} `json:"metadata"`
	}
	if err := json.Unmarshal(snapshot, &cfg); err != nil {
		// core_init reports an unreadable snapshot with the detail this
		// function does not have.
		return false, nil //nolint:nilerr
	}
	for _, source := range cfg.Metadata.Sources {
		for _, table := range source.Tables {
			if len(table.Attachments) > 0 {
				return true, nil
			}
		}
	}
	return false, nil
}

// withSecrets adds the deployment secrets to the snapshot the core is seeded
// with.
//
// They are merged here rather than written into `core-config.json`, because
// that file is committed and shipped inside a binary, and a signing key that
// travels with the artifact is a key everyone who has the artifact holds.
func withSecrets(snapshot []byte, secrets map[string]string) ([]byte, error) {
	if len(secrets) == 0 || len(snapshot) == 0 {
		return snapshot, nil
	}
	var cfg map[string]json.RawMessage
	if err := json.Unmarshal(snapshot, &cfg); err != nil {
		// Leave it to core_init to report, with the detail this has not got.
		return snapshot, nil //nolint:nilerr
	}
	raw, err := json.Marshal(secrets)
	if err != nil {
		return nil, fmt.Errorf("encoding storage secrets: %w", err)
	}
	cfg["secrets"] = raw
	return json.Marshal(cfg)
}
