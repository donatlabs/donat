package donat

import (
	"bytes"
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

// The core resolves entropy through a host import because
// wasm32-unknown-unknown has no OS to ask, and a planner that signs an upload
// key needs unpredictable bytes at the moment it signs. A core that failed to
// link the import would not instantiate at all, so the interesting property to
// pin is that the host side actually fills the requested memory with fresh
// randomness rather than leaving it zeroed.
func TestHostRandomBytesFillsDistinctMemory(t *testing.T) {
	ctx := context.Background()
	core, err := newWasmCore(ctx)
	if err != nil {
		t.Fatalf("newWasmCore: %v", err)
	}
	defer core.close(ctx)

	const n = 32
	ptr, err := core.alloc(ctx, n)
	if err != nil {
		t.Fatalf("alloc: %v", err)
	}
	defer core.dealloc(ctx, ptr, n)

	read := func() []byte {
		if rc := randomBytes(ctx, core.mod, ptr, n); rc != 0 {
			t.Fatalf("randomBytes returned %d, want 0", rc)
		}
		b, ok := core.mod.Memory().Read(ptr, n)
		if !ok {
			t.Fatal("memory read out of range")
		}
		// Memory().Read aliases the instance's memory, so the bytes must be
		// copied before the next call overwrites them.
		return append([]byte(nil), b...)
	}

	first := read()
	if bytes.Equal(first, make([]byte, n)) {
		t.Fatal("host filled the buffer with zeros; the core would sign a predictable upload key")
	}
	if second := read(); bytes.Equal(first, second) {
		t.Fatal("two fills returned identical bytes; entropy is not per-call")
	}
}

// A zero-length fill is a no-op the host must accept, because refusing it
// would turn a harmless call into a plan failure.
func TestHostRandomBytesAcceptsZeroLength(t *testing.T) {
	ctx := context.Background()
	core, err := newWasmCore(ctx)
	if err != nil {
		t.Fatalf("newWasmCore: %v", err)
	}
	defer core.close(ctx)

	if rc := randomBytes(ctx, core.mod, 0, 0); rc != 0 {
		t.Fatalf("randomBytes(0,0) returned %d, want 0", rc)
	}
}

// The host must refuse a fill it cannot place in memory rather than report
// success, or the core would proceed with whatever the buffer already held.
func TestHostRandomBytesRejectsOutOfRange(t *testing.T) {
	ctx := context.Background()
	core, err := newWasmCore(ctx)
	if err != nil {
		t.Fatalf("newWasmCore: %v", err)
	}
	defer core.close(ctx)

	beyond := core.mod.Memory().Size()
	if rc := randomBytes(ctx, core.mod, beyond, 16); rc == 0 {
		t.Fatal("randomBytes accepted a write past the end of linear memory")
	}
}
