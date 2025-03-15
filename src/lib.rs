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

#[repr(C)]
pub struct RawStack {
    pub start: *mut u8,
    pub end: *mut u8,
}

#[link(wasm_import_module = "asyncify")]
unsafe extern "C" {
    pub unsafe fn start_unwind(stack: *mut RawStack);
    pub unsafe fn stop_unwind();
    pub unsafe fn start_rewind(stack: *mut RawStack);
    pub unsafe fn stop_rewind();
    pub unsafe fn get_state() -> u32;
}

pub struct RawCore {
    waker: AtomicWaker,
    pub stack: UnsafeCell<RawStack>,
    needs_rewind: AtomicBool,
}

impl RawCore {
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
    pub fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        unsafe { self.embed_internal(fut).assume_init() }
    }
}
pub struct RawCoroutine<U, T> {
    raw: *const RawCore,
    state: MaybeUninit<MaybeUninit<U>>,
    r#fn: fn(&MaybeUninit<U>) -> MaybeUninit<T>,
}
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