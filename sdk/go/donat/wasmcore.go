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
// single-threaded; the host pools them (later phase).
type wasmCore struct {
	runtime   wazero.Runtime
	mod       api.Module
	abiVer    api.Function
	allocFn   api.Function
	deallocFn api.Function
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
	}
	if c.abiVer == nil || c.allocFn == nil || c.deallocFn == nil {
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
