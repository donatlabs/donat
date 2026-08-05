package donat

import (
	"context"
	"crypto/rand"
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
	shapeFn   api.Function
	lastErrFn api.Function
}

// hostModuleName is the wasm import module the core resolves its host
// capabilities through. It must match `wasm_import_module` in
// crates/wasm-core/src/rng.rs.
const hostModuleName = "donat_host"

// randomBytes fills len bytes of the instance's linear memory at ptr with
// cryptographically secure randomness, returning 0 on success.
//
// The core needs entropy because planning a file upload mints the object key
// that the presigned signature covers, and it must do so while it signs.
// wasm32-unknown-unknown has no OS to ask, so the host supplies it here and
// the planner keeps calling Uuid::new_v4() exactly as the native engine does.
func randomBytes(_ context.Context, mod api.Module, ptr, size uint32) uint32 {
	if size == 0 {
		return 0
	}
	buf := make([]byte, size)
	if _, err := rand.Read(buf); err != nil {
		// Reporting failure makes the core error out rather than sign a key
		// derived from whatever the buffer happened to hold.
		return 1
	}
	if !mod.Memory().Write(ptr, buf) {
		return 1
	}
	return 0
}

func newWasmCore(ctx context.Context) (*wasmCore, error) {
	rt := wazero.NewRuntime(ctx)
	if _, err := rt.NewHostModuleBuilder(hostModuleName).
		NewFunctionBuilder().
		WithFunc(randomBytes).
		Export("random_bytes").
		Instantiate(ctx); err != nil {
		_ = rt.Close(ctx)
		return nil, fmt.Errorf("instantiate %s host module: %w", hostModuleName, err)
	}
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
		shapeFn:   mod.ExportedFunction("core_shape_action"),
		lastErrFn: mod.ExportedFunction("core_last_error"),
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

// initState seeds the wasm instance with serialized metadata+catalog JSON and
// compiles the serving snapshot. cfgJSON must be
// {"metadata":<Metadata>,"catalog":<Catalog>}.
//
// core_init distinguishes a config the host failed to serialize (1) from
// declarative metadata that did not compile (2). The second is the
// deployment's own rules, commands or permissions, and its message comes back
// through core_last_error — a bare exit code would leave an operator with a
// directory of command files and no idea which one is wrong.
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
	switch res[0] {
	case 0:
		return nil
	case 2:
		return fmt.Errorf("metadata did not compile: %s", c.lastError(ctx))
	default:
		return fmt.Errorf("core_init returned %d: %s", res[0], c.lastError(ctx))
	}
}

// lastError reads the message behind the most recent failed core_init. It is
// best-effort: a host that cannot read it still reports the exit code.
func (c *wasmCore) lastError(ctx context.Context) string {
	if c.lastErrFn == nil {
		return "no detail available (core.wasm predates core_last_error)"
	}
	res, err := c.lastErrFn.Call(ctx)
	if err != nil {
		return fmt.Sprintf("could not read detail: %v", err)
	}
	ptr, n := uint32(res[0]>>32), uint32(res[0]&0xffffffff)
	if n == 0 {
		return "no detail reported"
	}
	defer c.dealloc(ctx, ptr, n) //nolint:errcheck
	buf, ok := c.mod.Memory().Read(ptr, n)
	if !ok {
		return "detail pointer out of range"
	}
	return string(buf)
}

// compile sends inputJSON to the wasm core and returns the PlanV1 JSON.
// Both the input buffer and the wasm-side output buffer are dealloc'd before
// returning; the returned slice is owned by the caller.
func (c *wasmCore) compile(ctx context.Context, inputJSON []byte) ([]byte, error) {
	return c.callJSON(ctx, c.compileFn, inputJSON)
}

// callJSON writes inputJSON into linear memory, calls fn, and copies the
// packed (ptr<<32|len) result out. Both buffers are freed before returning;
// the returned slice is owned by the caller.
func (c *wasmCore) callJSON(ctx context.Context, fn api.Function, inputJSON []byte) ([]byte, error) {
	n := uint32(len(inputJSON))
	inPtr, err := c.alloc(ctx, n)
	if err != nil {
		return nil, fmt.Errorf("alloc: %w", err)
	}
	defer c.dealloc(ctx, inPtr, n) //nolint:errcheck
	if ok := c.mod.Memory().Write(inPtr, inputJSON); !ok {
		return nil, fmt.Errorf("memory write out of range")
	}
	res, err := fn.Call(ctx, uint64(inPtr), uint64(n))
	if err != nil {
		return nil, fmt.Errorf("call: %w", err)
	}
	packed := res[0]
	outPtr := uint32(packed >> 32)
	outLen := uint32(packed)
	data, ok := c.mod.Memory().Read(outPtr, outLen)
	if !ok {
		return nil, fmt.Errorf("cannot read output at ptr=%d len=%d", outPtr, outLen)
	}
	// Copy the bytes out before dealloc-ing the output buffer.
	out := make([]byte, outLen)
	copy(out, data)
	if err := c.dealloc(ctx, outPtr, outLen); err != nil {
		return nil, fmt.Errorf("dealloc output: %w", err)
	}
	return out, nil
}

// shapeAction sends the collected action results to the core and returns the
// shaped response JSON. Same buffer discipline as compile.
func (c *wasmCore) shapeAction(ctx context.Context, inputJSON []byte) ([]byte, error) {
	if c.shapeFn == nil {
		return nil, fmt.Errorf("core.wasm predates core_shape_action")
	}
	return c.callJSON(ctx, c.shapeFn, inputJSON)
}
