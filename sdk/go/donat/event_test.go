package donat

import (
	"testing"

	"github.com/shopspring/decimal"
)

type row struct {
	C1 int32  `json:"c1"`
	C2 string `json:"c2"`
}

const insertEnvelope = `{
  "id": "ec1c2e0a-0000-0000-0000-000000000000",
  "created_at": "2026-06-13T10:00:00.000000+00:00",
  "table": { "schema": "hge_tests", "name": "test_t1" },
  "trigger": { "name": "t1_all" },
  "event": {
    "op": "INSERT",
    "data": { "old": null, "new": { "c1": 1, "c2": "hello" } },
    "session_variables": null
  },
  "delivery_info": { "current_retry": 0, "max_retries": 0 }
}`

const deleteEnvelope = `{
  "id": "ec1c2e0a-0000-0000-0000-000000000001",
  "created_at": "2026-06-13T10:00:00.000000+00:00",
  "table": { "schema": "hge_tests", "name": "test_t1" },
  "trigger": { "name": "t1_all" },
  "event": {
    "op": "DELETE",
    "data": { "old": { "c1": 1, "c2": "world" }, "new": null },
    "session_variables": null
  },
  "delivery_info": { "current_retry": 0, "max_retries": 0 }
}`

func TestParseEventInsert(t *testing.T) {
	ev, err := ParseEvent[row]([]byte(insertEnvelope))
	if err != nil {
		t.Fatalf("ParseEvent: %v", err)
	}
	if ev.Op != OpInsert {
		t.Errorf("Op = %q, want INSERT", ev.Op)
	}
	if ev.Old != nil {
		t.Errorf("Old = %+v, want nil on INSERT", ev.Old)
	}
	if ev.New == nil || ev.New.C1 != 1 || ev.New.C2 != "hello" {
		t.Errorf("New = %+v, want {1 hello}", ev.New)
	}
	if ev.Table.Schema != "hge_tests" || ev.Trigger.Name != "t1_all" {
		t.Errorf("table/trigger = %+v/%+v", ev.Table, ev.Trigger)
	}
}

func TestParseEventDelete(t *testing.T) {
	ev, err := ParseEvent[row]([]byte(deleteEnvelope))
	if err != nil {
		t.Fatalf("ParseEvent: %v", err)
	}
	if ev.Op != OpDelete {
		t.Errorf("Op = %q, want DELETE", ev.Op)
	}
	if ev.New != nil {
		t.Errorf("New = %+v, want nil on DELETE", ev.New)
	}
	if ev.Old == nil || ev.Old.C2 != "world" {
		t.Errorf("Old = %+v, want {1 world}", ev.Old)
	}
}

type money struct {
	Amount decimal.Decimal `json:"amount"`
}

func TestParseEventNumericPrecision(t *testing.T) {
	const env = `{
      "id": "x", "created_at": "2026-06-13T10:00:00.000000+00:00",
      "table": {"schema":"public","name":"acct"}, "trigger": {"name":"t"},
      "event": {"op":"INSERT","data":{"old":null,"new":{"amount":12345678901234.56789}},"session_variables":null},
      "delivery_info": {"current_retry":0,"max_retries":0}
    }`
	ev, err := ParseEvent[money]([]byte(env))
	if err != nil {
		t.Fatalf("ParseEvent: %v", err)
	}
	if got := ev.New.Amount.String(); got != "12345678901234.56789" {
		t.Errorf("Amount = %s, want 12345678901234.56789 (no precision loss)", got)
	}
}
