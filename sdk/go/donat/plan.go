package donat

import (
	"encoding/json"
	"fmt"
)

// PlanKind is the discriminant of a compiled plan.
type PlanKind string

const (
	PlanQuery    PlanKind = "query"
	PlanMutation PlanKind = "mutation"
	PlanAction   PlanKind = "action"
	PlanErrorK   PlanKind = "error"
)

// Plan is the Go mirror of the Rust PlanV1 contract (crates/wasm-core/src/plan.rs).
type Plan struct {
	Kind        PlanKind
	Version     uint32
	Transaction bool
	Statements  []Statement
	Hooks       []Hook
	// Response is the top-level key order the client asked for. The host
	// builds the response object from it rather than from the statement
	// results alone, because a root __typename is answered by the planner
	// and never reaches SQL.
	Response []ResponseSlot
	ErrorMap map[string]string
	// IsQuery and Items are set when Kind == PlanAction. Query actions may
	// run concurrently; mutation actions must not, because a client that
	// ordered two writes in one operation gets that order.
	IsQuery bool
	Items   []ActionItem
	Err     *PlanErr // set when Kind == PlanErrorK
}

// ActionItem is one top-level field of an action operation. A "typename" item
// is answered by the core and never reaches a function.
type ActionItem struct {
	Kind  string `json:"kind"` // "typename" | "call"
	Alias string `json:"alias"`
	Value string `json:"value"` // typename only

	Name             string          `json:"name"`
	Input            json.RawMessage `json:"input"`
	SessionVariables json.RawMessage `json:"session_variables"`
	// Handler nil means the action is resolved by a function this host
	// registered under Name; a value is a webhook URL.
	Handler            *string `json:"handler"`
	Timeout            *uint64 `json:"timeout"`
	ForwardClientHeads bool    `json:"forward_client_headers"`
}

// ResponseSlot is one top-level response key. Kind is "source_field" when the
// value comes from a statement result, or "local_typename" when the planner
// already resolved it.
type ResponseSlot struct {
	Kind  string `json:"kind"`
	Key   string `json:"key"`
	Value string `json:"value"`
}

// Statement is one SQL statement in a plan.
type Statement struct {
	Alias  string            `json:"alias"`
	SQL    string            `json:"sql"`
	Params []json.RawMessage `json:"params"`
	// Result is how this statement's row must be read. Empty means the
	// ordinary shape: one value in column 0. See ResultCommandExecution.
	Result string `json:"result"`
}

// Result shapes a statement can declare.
const (
	// ResultValue is the default and the absent value: one JSON or text value
	// in column 0.
	ResultValue = ""
	// ResultCommandExecution is an idempotent command's row: columns `root`,
	// `invocation_id` and `replayed`. A replay has to be distinguishable from
	// a first run, so the generation travels beside the result — and a host
	// that took column 0 would fail on the shape rather than on anything
	// meaningful.
	ResultCommandExecution = "command_execution"
)

// Hook is a post-commit event-trigger hook emitted by the wasm core.
type Hook struct {
	Phase string `json:"phase"`
	// Alias is the response key of the statement this hook's payload comes
	// from. A root may fire several triggers, so a trigger name does not
	// identify a result.
	Alias   string `json:"alias"`
	Trigger string `json:"trigger"`
	Schema  string `json:"schema"`
	Table   string `json:"table"`
	Op      string `json:"op"`
}

// PlanErr carries the structured error from a PlanErrorK plan.
type PlanErr struct {
	Code    string `json:"code"`
	Path    string `json:"path"`
	Message string `json:"message"`
}

// wirePlan matches the serde-tagged JSON: {"kind": "...", ...}.
type wirePlan struct {
	Kind        PlanKind          `json:"kind"`
	Version     uint32            `json:"version"`
	Transaction bool              `json:"transaction"`
	Statements  []Statement       `json:"statements"`
	Hooks       []Hook            `json:"hooks"`
	Response    []ResponseSlot    `json:"response"`
	ErrorMap    map[string]string `json:"error_map"`
	IsQuery     bool              `json:"is_query"`
	Items       []ActionItem      `json:"items"`
	Code        string            `json:"code"`
	Path        string            `json:"path"`
	Message     string            `json:"message"`
}

// decodePlan unmarshals a PlanV1 JSON payload produced by core_compile.
// It rejects non-error plans whose version != ABIVersion.
func decodePlan(raw []byte) (Plan, error) {
	var w wirePlan
	if err := json.Unmarshal(raw, &w); err != nil {
		return Plan{}, fmt.Errorf("decode plan: %w", err)
	}
	if w.Kind != PlanErrorK && w.Version != ABIVersion {
		return Plan{}, fmt.Errorf("plan version %d != supported %d", w.Version, ABIVersion)
	}
	p := Plan{
		Kind: w.Kind, Version: w.Version, Transaction: w.Transaction,
		Statements: w.Statements, Hooks: w.Hooks, Response: w.Response,
		ErrorMap: w.ErrorMap, IsQuery: w.IsQuery, Items: w.Items,
	}
	if w.Kind == PlanErrorK {
		p.Err = &PlanErr{Code: w.Code, Path: w.Path, Message: w.Message}
	}
	return p, nil
}
