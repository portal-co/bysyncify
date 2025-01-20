#![no_std]

use core::{
    cell::UnsafeCell,
    future::Future,
    hint::unreachable_unchecked,
    marker::PhantomData,
    mem::MaybeUninit,
    pin::Pin,
    sync::atomic::AtomicBool,
    task::{Context, Poll},
};

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use atomic_waker::AtomicWaker;
use waker_fn::waker_fn;

extern crate alloc;

#[repr(C)]
pub struct Stack {
    pub start: *mut u8,
    pub end: *mut u8,
    private: (),
}
impl Drop for Stack {
    fn drop(&mut self) {
        unsafe {
            Vec::from_raw_parts(
                self.start,
                self.end.byte_offset_from(self.start).try_into().unwrap(),
                self.end.byte_offset_from(self.start).try_into().unwrap(),
            );
        }
    }
}
impl Stack {
    pub fn new(mut a: Vec<u8>) -> Self {
        while a.len() != a.capacity() {
            a.push(0);
        }
        let mut a = a.leak();
        return Self {
            start: a.as_mut_ptr(),
            end: a.as_mut_ptr_range().end,
            private: (),
        };
    }
}

#[link(wasm_import_module = "asyncify")]
unsafe extern "C" {
    pub unsafe fn start_unwind(stack: *mut Stack);
    pub unsafe fn stop_unwind();
    pub unsafe fn start_rewind(stack: *mut Stack);
    pub unsafe fn stop_rewind();
    pub unsafe fn get_state() -> u32;
}

struct Core {
    waker: Arc<AtomicWaker>,
    stack: UnsafeCell<Stack>,
    needs_rewind: AtomicBool,
}

impl Core {
    fn new(stack_size: usize) -> Arc<Self> {
        Arc::new(Self {
            waker: Arc::new(Default::default()),
            stack: UnsafeCell::new(Stack::new((0..stack_size).map(|_| 0).collect())),
            needs_rewind: Default::default(),
        })
    }
    #[inline(never)]
    unsafe fn poll<T, U>(
        &self,
        cx: &mut Context,
        go: fn(U) -> MaybeUninit<T>,
        state: U,
    ) -> Poll<T> {
        self.waker.register(cx.waker());
        let mut r = false;
        unsafe {
            if self
                .needs_rewind
                .swap(false, core::sync::atomic::Ordering::SeqCst)
            {
                r = true;
                start_rewind(self.stack.get());
            }
            let v = go(state);
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
            let w = self.waker.clone();
            loop {
                let w = w.clone();
                match get_state() {
                    0 => match fut
                        .as_mut()
                        .poll(&mut Context::from_waker(&waker_fn(move || w.wake())))
                    {
                        Poll::Ready(a) => {
                            return MaybeUninit::new(a);
                        }
                        Poll::Pending => {
                            start_unwind(self.stack.get());
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
    fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        unsafe { self.embed_internal(fut).assume_init() }
    }
}
#[derive(Clone)]
pub struct CoreHandle<'a>(Arc<Core>, PhantomData<&'a ()>);
impl<'a> CoreHandle<'a> {
    pub fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        return self.0.embed(fut);
    }
    pub fn to_raw(&self) -> RawCoreHandle {
        RawCoreHandle(self.0.clone())
    }
}
#[derive(Clone)]
pub struct RawCoreHandle(Arc<Core>);
impl RawCoreHandle {
    pub unsafe fn to_handle<'a>(&self) -> CoreHandle<'a> {
        CoreHandle(self.0.clone(), PhantomData)
    }
    pub unsafe fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        return self.0.embed(fut);
    }
    pub fn with_safe_handle<T>(&self, x: impl FnOnce(CoreHandle<'_>) -> T) -> T {
        return x(CoreHandle(self.0.clone(), PhantomData));
    }
}
// pin_project_lite::pin_project! {
pub struct Coroutine<F> {
    // #[pin]
    core: Arc<Core>,
    fun: *mut F,
}
// }
impl<F: FnOnce(CoreHandle<'_>) -> T, T> Future for Coroutine<F> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // let mut this = self.project();
        // let h = CoreHandle(self.core.clone(), PhantomData);
        unsafe {
            self.core.poll(
                cx,
                |(a, h)| {
                    MaybeUninit::new(Box::from_raw(a as *mut F)(CoreHandle(
                        (&*h).clone(),
                        PhantomData,
                    )))
                },
                (self.fun as *mut (), &raw const self.core),
            )
        }
    }
}
impl<F: FnOnce(CoreHandle<'_>) -> T, T> Coroutine<F> {
    pub fn new(stack_size: usize, f: F) -> Self {
        Self {
            core: Core::new(stack_size),
            fun: Box::into_raw(Box::new(f)),
        }
    }
}
