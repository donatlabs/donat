// Command petshop-golang is a native Go event-trigger handler for the petshop
// schema. The Donat engine POSTs event-trigger envelopes to this server; the
// SDK decodes them into typed Go structs and routes them to the handlers you
// register. You own the HTTP server — your router, your middleware, your auth.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"log"
	"net/http"
	"os"

	"github.com/donatlabs/donat/examples/petshop-golang/gen"
	"github.com/donatlabs/donat/sdk/go/donat"
)

func main() {
	reg := donat.NewRegistry()

	// Register a typed handler per event trigger (names match the YAML
	// `event_triggers[].name` you declare in the petshop metadata).
	donat.On(reg, "on_order_placed", func(_ context.Context, ev donat.Event[gen.Orders]) error {
		switch ev.Op {
		case donat.OpInsert:
			log.Printf("order #%d placed by customer %s (status=%s)",
				ev.New.Id, ev.New.CustomerId, ev.New.Status)
		case donat.OpUpdate:
			if ev.Old.Status != ev.New.Status {
				log.Printf("order #%d: %s -> %s", ev.New.Id, ev.Old.Status, ev.New.Status)
			}
		}
		return nil
	})

	donat.On(reg, "on_pet_status", func(_ context.Context, ev donat.Event[gen.Pet]) error {
		if ev.New != nil && ev.New.Status == "sold" {
			log.Printf("pet %q (#%d) sold for %s", ev.New.Name, ev.New.Id, ev.New.Price.String())
		}
		return nil
	})

	// Plain net/http — nothing engine-specific about the transport. The SDK
	// only decodes the envelope and routes it to the right handler.
	mux := http.NewServeMux()
	mux.HandleFunc("POST /events", eventsHandler(reg))

	addr := os.Getenv("ADDR")
	if addr == "" {
		addr = ":8081"
	}
	log.Printf("petshop-golang listening on %s; handlers: %v", addr, reg.Names())
	log.Fatal(http.ListenAndServe(addr, mux))
}

// eventsHandler reads the envelope, extracts which trigger fired, and
// dispatches. HTTP status drives the engine's at-least-once retry contract:
// 2xx = acked, 5xx = retried. Handlers must be idempotent.
func eventsHandler(reg *donat.Registry) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		// The envelope says which trigger fired; route on that name.
		var meta struct {
			Trigger struct {
				Name string `json:"name"`
			} `json:"trigger"`
		}
		if err := json.Unmarshal(body, &meta); err != nil {
			http.Error(w, "malformed envelope", http.StatusBadRequest)
			return
		}

		switch err := reg.Dispatch(r.Context(), meta.Trigger.Name, body); {
		case err == nil:
			w.WriteHeader(http.StatusOK)
		case errors.Is(err, donat.ErrNoHandler):
			// No handler for this trigger: ack so the engine stops retrying.
			log.Printf("no handler for trigger %q; acking", meta.Trigger.Name)
			w.WriteHeader(http.StatusNoContent)
		default:
			// Real failure: 500 so the engine retries per retry_conf.
			log.Printf("handler error for %q: %v", meta.Trigger.Name, err)
			http.Error(w, err.Error(), http.StatusInternalServerError)
		}
	}
}
