package donat

import (
	"context"
	"testing"
)

func TestCoreABIVersionRoundTrip(t *testing.T) {
	ctx := context.Background()
	core, err := newWasmCore(ctx)
	if err != nil {
		t.Fatalf("newWasmCore: %v", err)
	}
	defer core.close(ctx)

	got, err := core.abiVersion(ctx)
	if err != nil {
		t.Fatalf("abiVersion: %v", err)
	}
	if got != ABIVersion {
		t.Fatalf("ABI mismatch: wasm=%d host=%d", got, ABIVersion)
	}
}

func TestCoreAllocDeallocRoundTrip(t *testing.T) {
	ctx := context.Background()
	core, err := newWasmCore(ctx)
	if err != nil {
		t.Fatalf("newWasmCore: %v", err)
	}
	defer core.close(ctx)

	ptr, err := core.alloc(ctx, 64)
	if err != nil {
		t.Fatalf("alloc: %v", err)
	}
	if ptr == 0 {
		t.Fatal("alloc returned null pointer")
	}
	if ok := core.mod.Memory().Write(ptr, []byte("hello wasm core")); !ok {
		t.Fatal("memory write out of range")
	}
	if err := core.dealloc(ctx, ptr, 64); err != nil {
		t.Fatalf("dealloc: %v", err)
	}
}
