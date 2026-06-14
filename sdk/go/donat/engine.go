package donat

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"sort"
	"sync"
)

// Config constructs an Engine. The Backend is supplied by the caller — the
// engine never opens connections itself (composability requirement).
type Config struct {
	Backend  Backend   // required: database backend (e.g. Postgres(pool))
	Metadata []byte    // serialized {"metadata":..., "catalog":...} for core_init
	Registry *Registry // optional: Spec 003 native event-trigger handlers
	PoolSize int       // wasm instance pool size (default 4)
}

// Engine is an embeddable Donat GraphQL engine backed by the wasm core.
type Engine struct {
	cfg      Config
	backend  Backend
	registry *Registry
	mu       sync.Mutex
	insts    []*wasmCore // idle wazero instances, each seeded by core_init
	cache    sync.Map    // planCacheKey -> Plan
}

// New constructs and returns a ready Engine. It pre-seeds one wasm instance
// to detect bad metadata/catalog blobs at startup.
func New(ctx context.Context, cfg Config) (*Engine, error) {
	if cfg.Backend == nil {
		return nil, fmt.Errorf("donat.New: Config.Backend is required")
	}
	if cfg.PoolSize == 0 {
		cfg.PoolSize = 4
	}
	e := &Engine{cfg: cfg, backend: cfg.Backend, registry: cfg.Registry}
	// Pre-seed one instance to fail fast on a bad metadata/catalog blob.
	c, err := e.newSeededInstance(ctx)
	if err != nil {
		return nil, err
	}
	e.insts = append(e.insts, c)
	return e, nil
}

func (e *Engine) newSeededInstance(ctx context.Context) (*wasmCore, error) {
	c, err := newWasmCore(ctx)
	if err != nil {
		return nil, err
	}
	if err := c.initState(ctx, e.cfg.Metadata); err != nil {
		_ = c.close(ctx)
		return nil, fmt.Errorf("core_init: %w", err)
	}
	return c, nil
}

func (e *Engine) acquire(ctx context.Context) (*wasmCore, error) {
	e.mu.Lock()
	if n := len(e.insts); n > 0 {
		c := e.insts[n-1]
		e.insts = e.insts[:n-1]
		e.mu.Unlock()
		return c, nil
	}
	e.mu.Unlock()
	return e.newSeededInstance(ctx)
}

func (e *Engine) release(c *wasmCore) {
	e.mu.Lock()
	if len(e.insts) < e.cfg.PoolSize {
		e.insts = append(e.insts, c)
		e.mu.Unlock()
		return
	}
	e.mu.Unlock()
	_ = c.close(context.Background())
}

// planCacheKey uniquely identifies a compiled plan. dialect is constant per
// Engine (set from backend.Dialect()) but included for correctness in case
// two engines with different backends share a sync.Map (they don't today, but
// the key must be self-contained).
type planCacheKey struct{ query, role, varsHash, sessHash, dialect string }

// compileInput is the Go mirror of the Rust CompileInput (crates/wasm-core/src/compile.rs).
// JSON field names match the Rust serde field names exactly.
// Variables uses omitempty so a nil map is omitted rather than sent as null;
// the Rust side has #[serde(default)] which yields an empty map when the field
// is absent, but serde cannot deserialize null into a Map.
// Dialect is the SQL flavour to generate; omitempty means an absent value
// defaults to "postgres" in the Rust core (Task 1 contract, byte-identical).
type compileInput struct {
	Query             string                     `json:"query"`
	OperationName     *string                    `json:"operation_name,omitempty"`
	Variables         map[string]json.RawMessage `json:"variables,omitempty"`
	SessionVars       map[string]string          `json:"session_vars"`
	StringifyNumerics bool                       `json:"stringify_numerics"`
	Dialect           string                     `json:"dialect,omitempty"`
}

// compilePlan runs the wasm core (or returns a cached Plan). The cache key
// includes query text, role, variables, all session vars, and dialect because
// the wasm core inlines literals into SQL at compile time and dialect affects
// the generated SQL.
func (e *Engine) compilePlan(ctx context.Context, in compileInput) (Plan, error) {
	// Set the dialect from the backend before caching or compiling.
	if in.Dialect == "" {
		in.Dialect = e.backend.Dialect()
	}
	key := planCacheKey{
		query:    in.Query,
		role:     in.SessionVars["x-donat-role"],
		varsHash: hashJSON(in.Variables),
		sessHash: hashMap(in.SessionVars),
		dialect:  in.Dialect,
	}
	if v, ok := e.cache.Load(key); ok {
		return v.(Plan), nil
	}
	c, err := e.acquire(ctx)
	if err != nil {
		return Plan{}, err
	}
	defer e.release(c)
	inJSON, err := json.Marshal(in)
	if err != nil {
		return Plan{}, err
	}
	out, err := c.compile(ctx, inJSON)
	if err != nil {
		return Plan{}, err
	}
	p, err := decodePlan(out)
	if err != nil {
		return Plan{}, err
	}
	e.cache.Store(key, p)
	return p, nil
}

// hashJSON returns a sha256 hex digest of the JSON-marshalled value.
// The map keys are sorted for determinism.
func hashJSON(v map[string]json.RawMessage) string {
	if len(v) == 0 {
		return ""
	}
	h := sha256.New()
	keys := make([]string, 0, len(v))
	for k := range v {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		h.Write([]byte(k))
		h.Write([]byte("="))
		h.Write(v[k])
		h.Write([]byte(";"))
	}
	return hex.EncodeToString(h.Sum(nil))
}

// hashMap returns a sha256 hex digest of the sorted key=value pairs of m.
func hashMap(m map[string]string) string {
	if len(m) == 0 {
		return ""
	}
	h := sha256.New()
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		h.Write([]byte(k))
		h.Write([]byte("="))
		h.Write([]byte(m[k]))
		h.Write([]byte(";"))
	}
	return hex.EncodeToString(h.Sum(nil))
}
