//! wasm ABI for the Donat conformance core.
//!
//! wasm32 cannot pass structs across the boundary — only linear memory and
//! i32 offsets. The host (Go/wazero) drives this surface:
//!   1. `core_alloc(len)` -> ptr; host writes `len` input bytes at `ptr`.
//!   2. `core_init(ptr, len)` loads serialized metadata + Catalog snapshot.
//!   3. `core_compile(ptr, len)` -> packed i64 (out_ptr<<32|out_len);
//!      host reads `out_len` bytes at `out_ptr`, then `core_dealloc`s it.
//!
//! All payloads are JSON byte buffers, so the wire format can evolve without
//! breaking the numeric ABI. Instances are single-threaded (one per pooled
//! wazero instance on the host side).

// public so the integration tests (tests/plan_snapshots.rs) can call compile()/PlanV1 directly
pub mod compile;
pub mod plan;

use std::cell::RefCell;

pub use compile::{compile, CompileInput, CoreState};

/// Bump on any breaking ABI/PlanV1-major change. The Go mirror asserts this.
pub const ABI_VERSION: i32 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn core_abi_version() -> i32 {
    ABI_VERSION
}

/// Allocate `len` bytes in linear memory and hand the pointer to the host.
/// The buffer is leaked; the host must return it via `core_dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn core_alloc(len: i32) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a buffer previously returned by `core_alloc`/`core_compile`.
#[allow(clippy::not_unsafe_ptr_arg_deref)] // wasm ABI: ptr/len written by the host via core_alloc; cannot be `unsafe fn`
#[unsafe(no_mangle)]
pub extern "C" fn core_dealloc(ptr: *mut u8, len: i32) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: ptr/len originate from core_alloc (Vec::with_capacity(len)); wasm32's
    //         dlmalloc returns exact-size allocations, so capacity == len holds here.
    unsafe {
        drop(Vec::from_raw_parts(ptr, 0, len as usize));
    }
}

thread_local! {
    static STATE: RefCell<Option<compile::CoreState>> = const { RefCell::new(None) };
}

/// Load serialized config into the instance. Input JSON:
/// `{ "metadata": <Metadata>, "catalog": <Catalog> }`.
/// Returns 0 on success, 1 on a deserialization error.
#[allow(clippy::not_unsafe_ptr_arg_deref)] // wasm ABI: ptr/len written by the host via core_alloc; cannot be `unsafe fn`
#[unsafe(no_mangle)]
pub extern "C" fn core_init(ptr: *mut u8, len: i32) -> i32 {
    // SAFETY: ptr/len originate from core_alloc; the host wrote `len` initialised
    //         bytes before calling core_init; the slice is confined to this call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    #[derive(serde::Deserialize)]
    struct Cfg {
        metadata: donat_metadata::Metadata,
        catalog: donat_catalog_types::Catalog,
    }
    match serde_json::from_slice::<Cfg>(bytes) {
        Ok(cfg) => {
            STATE.with(|s| {
                *s.borrow_mut() = Some(compile::CoreState {
                    metadata: cfg.metadata,
                    catalog: cfg.catalog,
                });
            });
            0
        }
        Err(_) => 1,
    }
}

/// Compile (query, vars, session) -> PlanV1 JSON.
///
/// Returns a packed i64: `(out_ptr << 32) | out_len`. The host reads
/// `out_len` bytes at `out_ptr` then calls `core_dealloc(out_ptr, out_len)`.
/// Requires a prior `core_init`; returns an error PlanV1 if uninitialised.
#[allow(clippy::not_unsafe_ptr_arg_deref)] // wasm ABI: ptr/len written by the host via core_alloc; cannot be `unsafe fn`
#[unsafe(no_mangle)]
pub extern "C" fn core_compile(ptr: *mut u8, len: i32) -> i64 {
    // SAFETY: ptr/len originate from core_alloc; host wrote len initialised bytes.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let out = STATE.with(|s| {
        let borrow = s.borrow();
        match borrow.as_ref() {
            None => err_json("validation-failed", "core not initialized"),
            Some(state) => match serde_json::from_slice::<compile::CompileInput>(bytes) {
                Ok(input) => serde_json::to_vec(&compile::compile(state, &input))
                    .unwrap_or_else(|_| err_json("validation-failed", "plan serialize failed")),
                Err(e) => err_json("validation-failed", &e.to_string()),
            },
        }
    });
    let out_len = out.len() as i64;
    let mut boxed = out.into_boxed_slice();
    let out_ptr = boxed.as_mut_ptr();
    std::mem::forget(boxed);
    (out_ptr as i64) << 32 | out_len
}

fn err_json(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "kind": "error",
        "version": plan::PLAN_VERSION,
        "code": code,
        "path": "$",
        "message": message,
    }))
    .unwrap()
}
