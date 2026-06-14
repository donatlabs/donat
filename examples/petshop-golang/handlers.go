package main

import (
	"context"
	"log"

	"github.com/donatlabs/donat/examples/petshop-golang/gen"
	"github.com/donatlabs/donat/sdk/go/donat"
)

// ─────────────────────────────────────────────────────────────────────────────
// Event-trigger handlers — THIS is the file you edit.
//
// Each handler is a plain Go function called IN-PROCESS right after the
// mutation's transaction commits — no webhook, no HTTP, no separate service.
// The trigger name passed to donat.On must match an `event_triggers[].name`
// in the YAML metadata (see metadata/databases/default/tables/*.yaml).
//
// The payload is decoded into the generated row struct (gen.*), so you get a
// typed Event[T] with the operation, the table/trigger identity, and the row.
//
// SDK v1 note: ev.New currently carries the mutation result shape, not the
// individual changed row — full old/new row capture is a planned follow-up.
// ev.Op, ev.Trigger and ev.Table are always accurate.
// ─────────────────────────────────────────────────────────────────────────────

// RegisterHandlers wires every event-trigger handler into the registry.
// Add your own with another donat.On(reg, "<trigger name>", <func>) line.
func RegisterHandlers(reg *donat.Registry) {
	donat.On(reg, "on_order_placed", onOrderPlaced)
	donat.On(reg, "on_pet_status", onPetStatus)
}

// onOrderPlaced fires on INSERT and on UPDATE of orders.status.
func onOrderPlaced(_ context.Context, ev donat.Event[gen.Orders]) error {
	log.Printf("[event] on_order_placed: op=%s table=%s", ev.Op, ev.Table.Name)
	// Your business logic here: enqueue a fulfilment job, send a confirmation
	// email, update a cache, call another service, ...
	return nil
}

// onPetStatus fires on UPDATE of pet.status (e.g. available -> sold).
func onPetStatus(_ context.Context, ev donat.Event[gen.Pet]) error {
	log.Printf("[event] on_pet_status: op=%s table=%s", ev.Op, ev.Table.Name)
	// Your business logic here.
	return nil
}
