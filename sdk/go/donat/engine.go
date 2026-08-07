package donat

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"net/http"
	"sort"
	"sync"
	"sync/atomic"
	"time"
)

// Config constructs an Engine. The Backend is supplied by the caller — the
// engine never opens connections itself (composability requirement).
type Config struct {
	Backend  Backend   // required: database backend (e.g. Postgres(pool))
	Metadata []byte    // serialized {"metadata":..., "catalog":...} for core_init
	Registry *Registry // optional: Spec 003 native event-trigger handlers
	// Functions implement the actions the metadata declares without a
	// `handler`. An action declared that way is resolved in this process,
	// so New refuses to start when one has no function: the field would be
	// in the schema and never work.
	Functions *Functions
	// HTTPClient calls actions that declare a webhook handler. Optional;
	// http.DefaultClient is used when unset.
	HTTPClient *http.Client
	// Secrets resolve the `value_from_env` references in the metadata's
	// storage configuration. They are supplied here rather than baked into
	// the snapshot, because a signing key has no business in a file that
	// ships inside a binary.
	Secrets map[string]string
	// ExternalBaseURL is the absolute prefix for engine-served URLs, e.g.
	// "https://api.example.com". Empty means same-origin.
	ExternalBaseURL string
	// Now overrides the clock URLs are signed with. Optional; time.Now is
	// used when unset.
	Now      func() time.Time
	PoolSize int // wasm instance pool size (default 4)
	// PlanCacheSize bounds the compiled-plan cache. Optional; 2048 when unset.
	PlanCacheSize int
	// OnHookError is called when a post-commit event handler could not be
	// reached — its payload failed to decode, or it returned an error. The
	// write has already committed, so nothing can be undone; what matters is
	// that the failure is not silent. A handler whose payload shape does not
	// match the row would otherwise simply never run, and look identical to
	// one with nothing to do. Optional; failures are logged when unset.
	OnHookError func(trigger string, err error)
	// Middleware wraps the GraphQL handler that Main serves, outermost first.
	// The engine authenticates nothing — it reads the role from the request —
	// so this is where a deployment decides who may reach it at all.
	Middleware []func(http.Handler) http.Handler
}

// Option configures a Config. The same options build the Config that Main
// serves from, so a program that outgrows Main can switch to New without
// rewriting its registrations.
type Option func(*Config)

// Engine is an embeddable Donat GraphQL engine backed by the wasm core.
type Engine struct {
	cfg      Config
	backend  Backend
	registry *Registry
	mu       sync.Mutex
	insts    []*wasmCore // idle wazero instances, each seeded by core_init
	cache    sync.Map    // planCacheKey -> Plan
	// signsURLs is true when the metadata declares a file attachment, which
	// makes every plan unique to its request and therefore uncacheable.
	signsURLs bool
	cacheSize atomic.Int64
	// live counts instances that exist, idle or in use, so the pool size is a
	// limit rather than a target. waiters are callers parked until a release.
	live    int
	waiters []chan struct{}
}

// defaultPlanCacheSize bounds the compiled-plan cache. Large enough that a
// normal application never evicts, small enough that a caller cannot grow the
// process by sending distinct variables.
const defaultPlanCacheSize = 2048

// now is the clock the engine signs with. Config.Now overrides it so a test
// can pin a day boundary.
func (e *Engine) now() time.Time {
	if e.cfg.Now != nil {
		return e.cfg.Now()
	}
	return time.Now()
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
	signs, err := snapshotDeclaresAttachments(cfg.Metadata)
	if err != nil {
		return nil, err
	}
	if err := checkFunctionsCoverActions(cfg.Metadata, cfg.Functions); err != nil {
		return nil, err
	}
	if err := checkFunctionsMatchDeclarations(cfg.Metadata, cfg.Functions); err != nil {
		return nil, err
	}
	e := &Engine{cfg: cfg, backend: cfg.Backend, registry: cfg.Registry, signsURLs: signs}
	// Pre-seed one instance to fail fast on a bad metadata/catalog blob. It
	// counts against the pool limit like any other.
	c, err := e.newSeededInstance(ctx)
	if err != nil {
		return nil, err
	}
	e.insts = append(e.insts, c)
	e.live = 1
	return e, nil
}

func (e *Engine) newSeededInstance(ctx context.Context) (*wasmCore, error) {
	c, err := newWasmCore(ctx)
	if err != nil {
		return nil, err
	}
	snapshot, err := withSecrets(e.cfg.Metadata, e.cfg.Secrets)
	if err != nil {
		_ = c.close(ctx)
		return nil, err
	}
	if err := c.initState(ctx, snapshot); err != nil {
		_ = c.close(ctx)
		return nil, fmt.Errorf("core_init: %w", err)
	}
	return c, nil
}

// acquire takes an idle instance, or waits for one when the pool is at its
// limit.
//
// Creating one is not cheap — it instantiates the module and re-runs
// `core_init` over the whole snapshot — so a burst of concurrent compiles used
// to create an unbounded number of them, each competing for the same cores.
// Blocking instead makes PoolSize mean what it says.
func (e *Engine) acquire(ctx context.Context) (*wasmCore, error) {
	for {
		e.mu.Lock()
		if n := len(e.insts); n > 0 {
			c := e.insts[n-1]
			e.insts = e.insts[:n-1]
			e.mu.Unlock()
			return c, nil
		}
		if e.live < e.cfg.PoolSize {
			e.live++
			e.mu.Unlock()
			c, err := e.newSeededInstance(ctx)
			if err != nil {
				e.mu.Lock()
				e.live--
				e.mu.Unlock()
				return nil, err
			}
			return c, nil
		}
		// At the limit: wait for a release rather than adding another.
		wait := make(chan struct{})
		e.waiters = append(e.waiters, wait)
		e.mu.Unlock()
		select {
		case <-wait:
		case <-ctx.Done():
			e.abandonWait(wait)
			return nil, ctx.Err()
		}
	}
}

// abandonWait takes a cancelled caller's channel out of the queue.
//
// Leaving it there is not merely untidy: `release` wakes the last waiter it
// finds, so an abandoned channel absorbs a release meant for somebody still
// waiting — and that caller then blocks forever while the instance sits idle
// in `e.insts`.
//
// If the channel is already gone, a release reached it first. The wake cannot
// be used and must not be dropped either, so it is handed to the next waiter.
func (e *Engine) abandonWait(wait chan struct{}) {
	e.mu.Lock()
	defer e.mu.Unlock()
	for i, w := range e.waiters {
		if w == wait {
			e.waiters = append(e.waiters[:i], e.waiters[i+1:]...)
			return
		}
	}
	if n := len(e.waiters); n > 0 {
		next := e.waiters[n-1]
		e.waiters = e.waiters[:n-1]
		close(next)
	}
}

func (e *Engine) release(c *wasmCore) {
	e.mu.Lock()
	e.insts = append(e.insts, c)
	if n := len(e.waiters); n > 0 {
		wait := e.waiters[n-1]
		e.waiters = e.waiters[:n-1]
		close(wait)
	}
	e.mu.Unlock()
}

// planCacheKey uniquely identifies a compiled plan. dialect is constant per
// Engine (set from backend.Dialect()) but included for correctness in case
// two engines with different backends share a sync.Map (they don't today, but
// the key must be self-contained).
type planCacheKey struct{ query, operation, role, varsHash, sessHash, dialect string }

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
	// Now is the instant this request signs file URLs as of, RFC 3339. wasm
	// has no clock, and a signature carries a day-scoped key and an expiry,
	// so the time travels with the request.
	Now string `json:"now,omitempty"`
	// ExternalBaseURL is the absolute prefix for engine-served URLs. Empty
	// means same-origin, which is what a browser needs by default.
	ExternalBaseURL string `json:"external_base_url,omitempty"`
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
	if in.Now == "" {
		in.Now = e.now().UTC().Format(time.RFC3339)
	}
	if in.ExternalBaseURL == "" {
		in.ExternalBaseURL = e.cfg.ExternalBaseURL
	}
	// A plan that signs a URL must never be reused. The signature covers a
	// one-shot upload id and a day-scoped key, so a cached plan would hand
	// two callers the same staging object and would keep serving yesterday's
	// key after the day rolls over. Deployments with no attachments — the
	// common case — cache exactly as before.
	if e.signsURLs {
		return e.compileUncached(ctx, in)
	}
	// Two operations in one document share its text, so the name is part of
	// the identity of the plan.
	operation := ""
	if in.OperationName != nil {
		operation = *in.OperationName
	}
	key := planCacheKey{
		query:     in.Query,
		operation: operation,
		role:      in.SessionVars["x-donat-role"],
		varsHash:  hashJSON(in.Variables),
		sessHash:  hashMap(in.SessionVars),
		dialect:   in.Dialect,
	}
	if v, ok := e.cache.Load(key); ok {
		return v.(Plan), nil
	}
	p, err := e.compileUncached(ctx, in)
	if err != nil {
		return Plan{}, err
	}
	e.cacheStore(key, p)
	return p, nil
}

// cacheStore adds a plan, evicting everything when the cache is full.
//
// The key includes the variables and every session variable, so the entries
// are per user and per argument: a caller sending unique values grows it
// without limit, which on a long-lived process is a slow leak. Clearing
// wholesale rather than evicting the least-used keeps this to a few lines and
// costs a recompile per entry afterwards — which is what the cache saves,
// once, and not what it exists for.
func (e *Engine) cacheStore(key planCacheKey, p Plan) {
	if e.cacheSize.Add(1) > int64(e.planCacheLimit()) {
		e.cache.Range(func(k, _ any) bool {
			e.cache.Delete(k)
			return true
		})
		e.cacheSize.Store(1)
	}
	e.cache.Store(key, p)
}

func (e *Engine) planCacheLimit() int {
	if e.cfg.PlanCacheSize > 0 {
		return e.cfg.PlanCacheSize
	}
	return defaultPlanCacheSize
}

// compileUncached runs the wasm core without consulting or filling the cache.
func (e *Engine) compileUncached(ctx context.Context, in compileInput) (Plan, error) {
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
	return decodePlan(out)
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
