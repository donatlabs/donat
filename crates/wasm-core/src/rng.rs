//! The wasm build's entropy source.
//!
//! Compiling the planner to `wasm32-unknown-unknown` leaves it with no way to
//! reach an operating system: the module is instantiated with no WASI and no
//! ambient capability. That matters because planning a file upload mints the
//! object key the presigned signature covers (`donat-storage`'s
//! `upload_target`), so the planner needs unpredictable bytes at the moment it
//! signs — not later, in the host.
//!
//! Rather than fork the planner for wasm and pass an id across the ABI, the
//! host exports the entropy as an ordinary wasm import and `getrandom` is
//! pointed at it (`getrandom_backend="custom"`, set for this target in
//! `.cargo/config.toml`). Every crate above keeps calling `Uuid::new_v4()`
//! exactly as the native engine does, so the wasm core and `donat-server`
//! remain the same code path.
//!
//! A host that does not export `donat_host.random_bytes` fails to instantiate
//! the module at all, which is the intended failure: an embedder cannot
//! silently end up minting guessable upload keys.

#[link(wasm_import_module = "donat_host")]
unsafe extern "C" {
    /// Fill `len` bytes at `ptr` with cryptographically secure randomness.
    /// Returns 0 on success and non-zero if the host could not supply it.
    fn random_bytes(ptr: *mut u8, len: usize) -> u32;
}

/// The symbol `getrandom`'s custom backend links against. The name is fixed by
/// `getrandom` and is still the `v03` spelling in 0.4.
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    // A zero-length fill is trivially satisfied, and calling the host with a
    // null-ish pointer for it would be gratuitous.
    if len == 0 {
        return Ok(());
    }
    match unsafe { random_bytes(dest, len) } {
        0 => Ok(()),
        // The host refused or failed. Reporting failure makes the caller
        // error rather than proceed with whatever the buffer happened to
        // hold, which for an upload key would be a security defect.
        _ => Err(getrandom::Error::UNSUPPORTED),
    }
}
