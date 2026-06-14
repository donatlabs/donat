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
	PlanErrorK   PlanKind = "error"
)

// Plan is the Go mirror of the Rust PlanV1 contract (crates/wasm-core/src/plan.rs).
type Plan struct {
	Kind        PlanKind
	Version     uint32
	Transaction bool
	Statements  []Statement
	Hooks       []Hook
	ErrorMap    map[string]string
	Err         *PlanErr // set when Kind == PlanErrorK
}

// Statement is one SQL statement in a plan.
type Statement struct {
	Alias  string            `json:"alias"`
	SQL    string            `json:"sql"`
	Params []json.RawMessage `json:"params"`
}

// Hook is a post-commit event-trigger hook emitted by the wasm core.
type Hook struct {
	Phase   string `json:"phase"`
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
	ErrorMap    map[string]string `json:"error_map"`
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
		Statements: w.Statements, Hooks: w.Hooks, ErrorMap: w.ErrorMap,
	}
	if w.Kind == PlanErrorK {
		p.Err = &PlanErr{Code: w.Code, Path: w.Path, Message: w.Message}
	}
	return p, nil
}
