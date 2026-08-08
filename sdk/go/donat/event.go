// Package donat is the pure-Go SDK for handling Donat event triggers in a Go
// process. It has no cgo dependency: `go get` and compile into your binary.
package donat

import (
	"encoding/json"
	"time"
)

// Op is the row operation that produced an event.
type Op string

const (
	OpInsert Op = "INSERT"
	OpUpdate Op = "UPDATE"
	OpDelete Op = "DELETE"
)

// TableRef identifies the table an event came from.
type TableRef struct {
	Schema string `json:"schema"`
	Name   string `json:"name"`
}

// TriggerRef identifies the firing trigger.
type TriggerRef struct {
	Name string `json:"name"`
}

// DeliveryInfo carries retry bookkeeping from the delivery layer.
type DeliveryInfo struct {
	CurrentRetry int `json:"current_retry"`
	MaxRetries   int `json:"max_retries"`
}

// Event is a decoded Donat event-trigger payload. T is a generated row struct.
// Old is nil on INSERT; New is nil on DELETE.
type Event[T any] struct {
	ID        string
	CreatedAt time.Time
	Table     TableRef
	Trigger   TriggerRef
	Op        Op
	Old       *T
	New       *T
	Session   map[string]string
	Delivery  DeliveryInfo
}

// wireEnvelope mirrors the on-the-wire Donat envelope (nested under "event").
type wireEnvelope[T any] struct {
	ID        string     `json:"id"`
	CreatedAt time.Time  `json:"created_at"`
	Table     TableRef   `json:"table"`
	Trigger   TriggerRef `json:"trigger"`
	Event     struct {
		Op   Op `json:"op"`
		Data struct {
			Old *T `json:"old"`
			New *T `json:"new"`
		} `json:"data"`
		SessionVariables map[string]string `json:"session_variables"`
	} `json:"event"`
	DeliveryInfo DeliveryInfo `json:"delivery_info"`
}

// ParseEvent decodes a raw Donat event-trigger envelope into Event[T].
func ParseEvent[T any](raw []byte) (Event[T], error) {
	var w wireEnvelope[T]
	if err := json.Unmarshal(raw, &w); err != nil {
		return Event[T]{}, err
	}
	return Event[T]{
		ID:        w.ID,
		CreatedAt: w.CreatedAt,
		Table:     w.Table,
		Trigger:   w.Trigger,
		Op:        w.Event.Op,
		Old:       w.Event.Data.Old,
		New:       w.Event.Data.New,
		Session:   w.Event.SessionVariables,
		Delivery:  w.DeliveryInfo,
	}, nil
}
