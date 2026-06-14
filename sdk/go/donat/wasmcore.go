package donat

import (
	"context"
	_ "embed"
	"fmt"

	"github.com/tetratelabs/wazero"
	"github.com/tetratelabs/wazero/api"
)

//go:embed wasm/core.wasm
var coreWasm []byte

// ABIVersion is the wasm-core ABI/PlanV1 major this host speaks. It must
// match core_abi_version() in the loaded blob.
const ABIVersion = 1

// wasmCore is a single instantiated wasm instance. Instances are
// single-threaded; the host pools them via Engine.
type wasmCore struct {
	runtime   wazero.Runtime
	mod       api.Module
	abiVer    api.Function
	allocFn   api.Function
	deallocFn api.Function
	initFn    api.Function
	compileFn api.Function
}

func newWasmCore(ctx context.Context) (*wasmCore, error) {
	rt := wazero.NewRuntime(ctx)
	mod, err := rt.Instantiate(ctx, coreWasm)
	if err != nil {
		_ = rt.Close(ctx)
		return nil, fmt.Errorf("instantiate core.wasm: %w", err)
	}
	c := &wasmCore{
		runtime:   rt,
		mod:       mod,
		abiVer:    mod.ExportedFunction("core_abi_version"),
		allocFn:   mod.ExportedFunction("core_alloc"),
		deallocFn: mod.ExportedFunction("core_dealloc"),
		initFn:    mod.ExportedFunction("core_init"),
		compileFn: mod.ExportedFunction("core_compile"),
	}
	if c.abiVer == nil || c.allocFn == nil || c.deallocFn == nil ||
		c.initFn == nil || c.compileFn == nil {
		_ = rt.Close(ctx)
		return nil, fmt.Errorf("core.wasm missing required exports")
	}
	return c, nil
}

func (c *wasmCore) abiVersion(ctx context.Context) (int32, error) {
	res, err := c.abiVer.Call(ctx)
	if err != nil {
		return 0, err
	}
	return int32(res[0]), nil
}

// alloc reserves len bytes in wasm memory and returns the pointer.
func (c *wasmCore) alloc(ctx context.Context, n uint32) (uint32, error) {
	res, err := c.allocFn.Call(ctx, uint64(n))
	if err != nil {
		return 0, err
	}
	return uint32(res[0]), nil
}

func (c *wasmCore) dealloc(ctx context.Context, ptr, n uint32) error {
	_, err := c.deallocFn.Call(ctx, uint64(ptr), uint64(n))
	return err
}

func (c *wasmCore) close(ctx context.Context) error {
	return c.runtime.Close(ctx)
}

// initState seeds the wasm instance with serialized metadata+catalog JSON.
// cfgJSON must be {"metadata":<Metadata>,"catalog":<Catalog>}.
// Returns an error if core_init returns non-zero (deserialization failure).
func (c *wasmCore) initState(ctx context.Context, cfgJSON []byte) error {
	n := uint32(len(cfgJSON))
	ptr, err := c.alloc(ctx, n)
	if err != nil {
		return fmt.Errorf("initState alloc: %w", err)
	}
	// Free the input buffer once core_init has consumed it.
	defer c.dealloc(ctx, ptr, n) //nolint:errcheck
	if ok := c.mod.Memory().Write(ptr, cfgJSON); !ok {
		return fmt.Errorf("initState memory write out of range")
	}
	res, err := c.initFn.Call(ctx, uint64(ptr), uint64(n))
	if err != nil {
		return fmt.Errorf("core_init call: %w", err)
	}
	if res[0] != 0 {
		return fmt.Errorf("core_init returned %d (metadata/catalog deserialisation failed)", res[0])
	}
	return nil
}

// compile sends inputJSON to the wasm core and returns the PlanV1 JSON.
// Both the input buffer and the wasm-side output buffer are dealloc'd before
// returning; the returned slice is owned by the caller.
func (c *wasmCore) compile(ctx context.Context, inputJSON []byte) ([]byte, error) {
	n := uint32(len(inputJSON))
	inPtr, err := c.alloc(ctx, n)
	if err != nil {
		return nil, fmt.Errorf("compile alloc: %w", err)
	}
	// Free the input buffer once core_compile has consumed it (fires at return).
	defer c.dealloc(ctx, inPtr, n) //nolint:errcheck
	if ok := c.mod.Memory().Write(inPtr, inputJSON); !ok {
		return nil, fmt.Errorf("compile memory write out of range")
	}
	res, err := c.compileFn.Call(ctx, uint64(inPtr), uint64(n))
	if err != nil {
		return nil, fmt.Errorf("core_compile call: %w", err)
	}
	packed := res[0]
	outPtr := uint32(packed >> 32)
	outLen := uint32(packed)
	data, ok := c.mod.Memory().Read(outPtr, outLen)
	if !ok {
		return nil, fmt.Errorf("compile: cannot read output at ptr=%d len=%d", outPtr, outLen)
	}
	// Copy the bytes out before dealloc-ing the output buffer.
	out := make([]byte, outLen)
	copy(out, data)
	if err := c.dealloc(ctx, outPtr, outLen); err != nil {
		return nil, fmt.Errorf("compile dealloc output: %w", err)
	}
	return out, nil
}
