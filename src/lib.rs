//! # bysyncify
//!
//! Bindings and a high-level interface to Binaryen's Asyncify transformation for WebAssembly.
//!
//! Asyncify is a feature of Binaryen that allows WebAssembly code to pause and resume execution,
//! enabling async/await style programming in environments that don't natively support it.
//! This crate provides Rust bindings and abstractions for working with Asyncify.
//!
//! ## Overview
//!
//! This crate provides two levels of abstraction:
//!
//! - **Low-level API**: [`RawStack`], [`RawCore`], and [`RawCoroutine`] provide direct access
//!   to Asyncify primitives with minimal overhead.
//! - **High-level API** (requires `alloc` feature): `Core`, `Coroutine`, and `CoreHandle`
//!   provide a safer, more ergonomic interface with automatic memory management.
//!
//! ## Features
//!
//! - `alloc`: Enables heap-allocated types like `Core`, `Coroutine`, and `CoreHandle`.
//!   This requires the `alloc` crate.
//!
//! ## Usage
//!
//! This crate is designed for WebAssembly targets that have been processed with Binaryen's
//! Asyncify transformation. The asyncify imports must be available at runtime.
//!
//! ### Basic Example (with `alloc` feature)
//!
//! ```ignore
//! use bysyncify::Coroutine;
//!
//! // Create a coroutine with a 1KB stack
//! let coroutine = Coroutine::new(1024, |handle| {
//!     // Use handle.embed() to await futures within the coroutine
//!     // handle.embed(some_future);
//!     42
//! });
//!
//! // Poll the coroutine as a future
//! // let result = coroutine.await;
//! ```

#![no_std]

use core::{
    cell::UnsafeCell,
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomData,
    mem::MaybeUninit,
    pin::Pin,
    sync::atomic::AtomicBool,
    task::{Context, Poll, Waker},
};

use atomic_waker::AtomicWaker;
// use waker_fn::waker_fn;

/// Raw stack structure used by Asyncify for storing execution state.
///
/// This structure represents a contiguous memory region that Asyncify uses
/// to save and restore the call stack during unwinding and rewinding operations.
///
/// # Layout
///
/// The structure is `#[repr(C)]` to ensure a stable memory layout compatible
/// with the Asyncify runtime.
#[repr(C)]
pub struct RawStack {
    /// Pointer to the beginning of the stack memory region.
    pub start: *mut u8,
    /// Pointer to the end of the stack memory region.
    pub end: *mut u8,
}

// FFI bindings to Asyncify runtime functions.
//
// These functions are imported from the "asyncify" WebAssembly module and provide
// the low-level primitives for stack unwinding and rewinding.
#[link(wasm_import_module = "asyncify")]
unsafe extern "C" {
    // Begins the stack unwinding process.
    //
    // This function initiates saving the current call stack state to the provided
    // stack buffer. After calling this, execution will unwind through all function
    // calls until control returns to the top level.
    //
    // # Safety
    //
    // - The `stack` pointer must point to a valid `RawStack` structure.
    // - The stack buffer must have sufficient capacity for the call stack.
    pub unsafe fn start_unwind(stack: *mut RawStack);

    // Completes the stack unwinding process.
    //
    // This function should be called after the stack has fully unwound to reset
    // the Asyncify state machine.
    //
    // # Safety
    //
    // Must only be called when Asyncify is in the unwinding state.
    pub unsafe fn stop_unwind();

    // Begins the stack rewinding process.
    //
    // This function initiates restoring a previously saved call stack from the
    // provided stack buffer. Execution will resume from where it was suspended.
    //
    // # Safety
    //
    // - The `stack` pointer must point to a valid `RawStack` structure.
    // - The stack must contain a previously unwound call stack.
    pub unsafe fn start_rewind(stack: *mut RawStack);

    // Completes the stack rewinding process.
    //
    // This function should be called after the stack has been fully restored.
    //
    // # Safety
    //
    // Must only be called when Asyncify is in the rewinding state.
    pub unsafe fn stop_rewind();

    // Returns the current Asyncify state.
    //
    // # Returns
    //
    // - `0`: Normal execution (not unwinding or rewinding)
    // - `1`: Currently unwinding
    // - `2`: Currently rewinding
    pub unsafe fn get_state() -> u32;
}

/// Core state manager for Asyncify operations.
///
/// `RawCore` manages the Asyncify stack and waker, coordinating the suspension
/// and resumption of coroutine execution. This is the low-level building block
/// for coroutine implementations.
///
/// # Usage
///
/// This type is typically not used directly. Instead, use `Core` or `Coroutine`
/// (available with the `alloc` feature) for a safer interface.
pub struct RawCore {
    waker: AtomicWaker,
    /// The Asyncify stack buffer used for storing suspended execution state.
    pub stack: UnsafeCell<RawStack>,
    needs_rewind: AtomicBool,
}

impl RawCore {
    /// Polls the coroutine, potentially unwinding or rewinding the stack.
    ///
    /// This method handles the Asyncify state machine, resuming execution if
    /// previously suspended and returning the result when complete.
    ///
    /// # Safety
    ///
    /// - The `go` function must be compatible with the current Asyncify state.
    /// - The `state` must contain valid initialization data for `go`.
    #[inline(never)]
    pub unsafe fn poll<T, U>(
        &self,
        cx: &mut Context,
        go: fn(&MaybeUninit<U>) -> MaybeUninit<T>,
        state: MaybeUninit<U>,
    ) -> Poll<T> {
        self.waker.register(cx.waker());
        let mut r = false;
        unsafe {
            if self
                .needs_rewind
                .swap(false, core::sync::atomic::Ordering::SeqCst)
            {
                r = true;
                start_rewind(core::mem::transmute(self.stack.get()));
            }
            let v = go(&state);
            match get_state() {
                0 => {
                    return Poll::Ready(v.assume_init());
                }
                1 => {
                    stop_unwind();
                    self.needs_rewind
                        .store(true, core::sync::atomic::Ordering::SeqCst);
                    return Poll::Pending;
                }
                _ => unreachable_unchecked(),
            }
        }
    }
    #[inline(never)]
    fn embed_internal<T>(
        &self,
        mut fut: Pin<&mut (dyn Future<Output = T> + '_)>,
    ) -> MaybeUninit<T> {
        unsafe {
            let w = &self.waker;
            loop {
                let w = w.clone();
                match get_state() {
                    0 => match fut.as_mut().poll(&mut Context::from_waker(&match w.take() {
                        Some(w) => w,
                        None => Waker::noop().clone(),
                    })) {
                        Poll::Ready(a) => {
                            return MaybeUninit::new(a);
                        }
                        Poll::Pending => {
                            start_unwind(core::mem::transmute(self.stack.get()));
                            return MaybeUninit::uninit();
                        }
                    },
                    2 => {
                        stop_rewind();
                    }
                    _ => unreachable_unchecked(),
                }
            }
        }
    }
    /// Embeds a future into the current Asyncify context.
    ///
    /// This method allows awaiting a future from within coroutine code by using
    /// Asyncify's stack unwinding mechanism to suspend execution when the future
    /// returns [`Poll::Pending`].
    ///
    /// # Arguments
    ///
    /// * `fut` - A pinned mutable reference to the future to embed.
    ///
    /// # Returns
    ///
    /// The output of the future once it completes.
    pub fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        unsafe { self.embed_internal(fut).assume_init() }
    }
}
impl awaiter_trait::Awaiter for RawCore{
    fn r#await<T>(&self, f: Pin<&mut dyn Future<Output = T>>) -> T {
        self.embed(f)
    }
}
awaiter_trait::autoimpl!(<> RawCore as Awaiter);
impl awaiter_trait_02::Awaiter for RawCore{
    fn r#await<T>(&self, f: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        self.embed(f)
    }
}
awaiter_trait_02::autoimpl!(<> RawCore as Awaiter);

/// A low-level coroutine that wraps a function with Asyncify support.
///
/// `RawCoroutine` implements [`Future`] and manages the execution of a coroutine
/// function that can suspend and resume using Asyncify.
///
/// # Type Parameters
///
/// * `U` - The type of the initial state/closure passed to the coroutine.
/// * `T` - The output type of the coroutine.
///
/// # Safety
///
/// This type requires careful handling of the raw pointer to [`RawCore`].
/// For a safer alternative, use `Coroutine` (available with the `alloc` feature).
pub struct RawCoroutine<U, T> {
    raw: *const RawCore,
    state: MaybeUninit<MaybeUninit<U>>,
    r#fn: fn(&MaybeUninit<U>) -> MaybeUninit<T>,
}

/// Creates a new raw coroutine.
///
/// This is a low-level function for creating coroutines. For most use cases,
/// prefer using `Coroutine::new` (available with the `alloc` feature).
///
/// # Safety
///
/// - The `raw` pointer must remain valid for the lifetime of the returned coroutine.
/// - The `fn` must be compatible with Asyncify transformation.
pub unsafe fn raw_cor_base<F, T>(raw: *const RawCore, f: F, r#fn: fn(&MaybeUninit<F>) -> MaybeUninit<T>) -> RawCoroutine<F, T> {
    RawCoroutine {
        raw,
        state: MaybeUninit::new(MaybeUninit::new(f)),
        r#fn,
    }
}
impl<U, T> Future for RawCoroutine<U, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            self.raw
                .as_ref()
                .unwrap()
                .poll(cx, self.r#fn, self.state.assume_init_read())
        }
    }
}
unsafe impl<U: Send, T> Send for RawCoroutine<U, T> {}
unsafe impl<U: Sync, T> Sync for RawCoroutine<U, T> {}

#[cfg(feature = "alloc")]
extern crate alloc;
#[cfg(feature = "alloc")]
pub mod alloc_support;
#[cfg(feature = "alloc")]
pub use alloc_support::*;