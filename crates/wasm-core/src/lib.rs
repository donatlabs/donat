//! wasm ABI for the Donat conformance core.
//!
//! wasm32 cannot pass structs across the boundary — only linear memory and
//! i32 offsets. The host (Go/wazero) drives this surface:
//!   1. `core_alloc(len)` -> ptr; host writes `len` input bytes at `ptr`.
//!   2. `core_init(ptr, len)` loads serialized metadata + Catalog snapshot.
//!   3. `core_compile(ptr, len)` -> packed i64 (out_ptr<<32 | out_len);
//!      host reads `out_len` bytes at `out_ptr`, then `core_dealloc`s it.
//! All payloads are JSON byte buffers, so the wire format can evolve without
//! breaking the numeric ABI. Instances are single-threaded (one per pooled
//! wazero instance on the host side).

mod compile;
mod plan;

use std::cell::RefCell;

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
#[unsafe(no_mangle)]
pub extern "C" fn core_dealloc(ptr: *mut u8, len: i32) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: ptr/len originate from `core_alloc` (capacity == len).
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
#[unsafe(no_mangle)]
pub extern "C" fn core_init(ptr: *mut u8, len: i32) -> i32 {
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
