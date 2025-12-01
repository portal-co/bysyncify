//! Heap-allocated types for Asyncify coroutines.
//!
//! This module provides higher-level, memory-managed wrappers around the low-level
//! Asyncify primitives. These types handle memory allocation and deallocation
//! automatically.

use core::alloc::Layout;

use crate::*;
use alloc::{boxed::Box, sync::Arc, vec::Vec};

/// Marker type for creating coroutines with a specific stack size.
///
/// This type implements the `awaiter_trait_02::Coroutine` trait, allowing it to
/// be used as a coroutine factory.
///
/// # Example
///
/// ```ignore
/// use bysyncify::CoroutimeMarker;
///
/// let marker = CoroutimeMarker { size: 4096 };
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct CoroutimeMarker {
    /// The size in bytes of the Asyncify stack buffer.
    pub size: usize,
}

/// A heap-allocated Asyncify stack.
///
/// This type owns the memory for an Asyncify stack buffer and ensures proper
/// deallocation when dropped.
#[repr(transparent)]
pub struct Stack {
    /// The underlying raw stack structure.
    pub raw: RawStack,
    private: (),
}
impl Drop for Stack {
    fn drop(&mut self) {
        unsafe {
            Vec::from_raw_parts(
                self.raw.start,
                self.raw
                    .end
                    .byte_offset_from(self.raw.start)
                    .try_into()
                    .unwrap(),
                self.raw
                    .end
                    .byte_offset_from(self.raw.start)
                    .try_into()
                    .unwrap(),
            );
        }
    }
}
impl Stack {
    /// Creates a new stack from a vector of bytes.
    ///
    /// The vector will be filled to capacity and then leaked to create a stable
    /// memory region for Asyncify operations.
    ///
    /// # Arguments
    ///
    /// * `a` - A vector that will be used as the backing storage for the stack.
    pub fn new(mut a: Vec<u8>) -> Self {
        while a.len() != a.capacity() {
            a.push(0);
        }
        let mut a = a.leak();
        return Self {
            raw: RawStack {
                start: a.as_mut_ptr(),
                end: a.as_mut_ptr_range().end,
            },
            private: (),
        };
    }
}

/// A heap-allocated Asyncify core with automatic memory management.
///
/// `Core` wraps [`RawCore`] and handles allocation and deallocation of the
/// Asyncify stack buffer.
pub struct Core {
    raw: RawCore,
}
impl Core {
    /// Creates a new core with the specified stack size.
    ///
    /// # Arguments
    ///
    /// * `size` - The size in bytes of the Asyncify stack buffer.
    ///
    /// # Panics
    ///
    /// Panics if memory allocation fails.
    pub fn new(size: usize) -> Self {
        let start = unsafe { alloc::alloc::alloc_zeroed(Layout::array::<u8>(size).unwrap()) };
        let end = unsafe { start.add(size) };
        Self {
            raw: RawCore {
                waker: AtomicWaker::default(),
                stack: UnsafeCell::new(RawStack { start, end }),
                needs_rewind: Default::default(),
            },
        }
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        let s = self.raw.stack.get_mut();
        unsafe {
            let len = s.end.byte_offset_from(s.start) as usize;
            alloc::alloc::dealloc(s.start, Layout::array::<u8>(len).unwrap());
        }
    }
}
pin_project_lite::pin_project! {
    /// A high-level coroutine with automatic memory management.
    ///
    /// `Coroutine` is the primary way to create and use Asyncify-powered coroutines.
    /// It implements [`Future`] and can be awaited in async contexts.
    ///
    /// # Type Parameters
    ///
    /// * `U` - The closure type that will be executed as the coroutine body.
    /// * `T` - The output type of the coroutine.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use bysyncify::Coroutine;
    /// use core::pin::pin;
    ///
    /// let coroutine = Coroutine::new(4096, |handle| {
    ///     // Embed futures within the coroutine
    ///     // let result = handle.embed(pin!(some_async_fn()));
    ///     42
    /// });
    ///
    /// // Poll the coroutine as a future
    /// // let result = coroutine.await;
    /// ```
    pub struct Coroutine<U,T>{
        #[pin]
        raw: RawCoroutine<(U,Arc<Core>),T>,
        keepalive: Arc<Core>
    }
}
impl<U: FnOnce(CoreHandle<'_>) -> T, T> Coroutine<U, T> {
    /// Creates a new coroutine with the specified stack size and body.
    ///
    /// # Arguments
    ///
    /// * `size` - The size in bytes of the Asyncify stack buffer.
    /// * `f` - The closure to execute as the coroutine body. The closure receives
    ///         a [`CoreHandle`] that can be used to embed futures.
    ///
    /// # Returns
    ///
    /// A new `Coroutine` that can be polled as a future.
    pub fn new(size: usize, f: U) -> Self {
        let c = Arc::new(Core::new(size));
        Self {
            raw: unsafe {
                crate::raw_cor_base(&c.raw, (f, c.clone()), |a| {
                    let (a, b) =
                        unsafe { core::mem::transmute::<_, &(MaybeUninit<U>, Arc<Core>)>(a) };
                    MaybeUninit::new(a.assume_init_read()(CoreHandle {
                        core: b.clone(),
                        phantom: PhantomData,
                    }))
                })
            },
            keepalive: c,
        }
    }
}
impl<U, T> Future for Coroutine<U, T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().raw.poll(cx)
    }
}
/// A handle to the coroutine's core, used to embed futures.
///
/// `CoreHandle` is passed to the coroutine body and provides the [`embed`](CoreHandle::embed)
/// method for awaiting futures within the coroutine context.
///
/// # Lifetime
///
/// The lifetime `'a` ensures that the handle cannot outlive the coroutine that
/// created it.
#[derive(Clone)]
pub struct CoreHandle<'a> {
    core: Arc<Core>,
    phantom: PhantomData<&'a ()>,
}
impl<'a> CoreHandle<'a> {
    /// Embeds a future within the coroutine, suspending execution until it completes.
    ///
    /// This method uses Asyncify to unwind the stack when the future returns
    /// [`Poll::Pending`] and rewind it when the coroutine is polled again.
    ///
    /// # Arguments
    ///
    /// * `fut` - A pinned mutable reference to the future to embed.
    ///
    /// # Returns
    ///
    /// The output of the future once it completes.
    pub fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        self.core.raw.embed(fut)
    }

    /// Converts this handle to a raw handle that can escape lifetime constraints.
    ///
    /// # Returns
    ///
    /// A [`RawCoreHandle`] that references the same core.
    pub fn raw(&self) -> RawCoreHandle {
        RawCoreHandle {
            core: self.core.clone(),
        }
    }
}
impl awaiter_trait::Awaiter for CoreHandle<'_> {
    fn r#await<T>(&self, f: Pin<&mut dyn Future<Output = T>>) -> T {
        self.embed(f)
    }
}
awaiter_trait::autoimpl!(<> CoreHandle<'_> as Awaiter);
impl awaiter_trait_02::Awaiter for CoreHandle<'_> {
    fn r#await<T>(&self, f: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        self.embed(f)
    }
}
awaiter_trait_02::autoimpl!(<> CoreHandle<'_> as Awaiter);
impl awaiter_trait_02::Coroutine for CoroutimeMarker {
    fn exec<T>(
        &self,
        f: impl FnOnce(&(dyn awaiter_trait_02::r#dyn::DynAwaiter + '_)) -> T,
    ) -> impl Future<Output = T> {
        Coroutine::new(self.size, move |a| f(&a))
    }
}
awaiter_trait_02::autoimpl!(<> CoroutimeMarker as Coroutine);

/// A raw handle to a coroutine's core without lifetime constraints.
///
/// `RawCoreHandle` is similar to [`CoreHandle`] but without lifetime tracking.
/// This allows it to be stored and used in contexts where a lifetime-bound
/// handle would not work.
///
/// # Safety
///
/// When using `RawCoreHandle`, the caller must ensure that the underlying
/// [`Core`] remains valid for the duration of use.
#[derive(Clone)]
pub struct RawCoreHandle {
    core: Arc<Core>,
}
impl RawCoreHandle {
    /// Converts this raw handle to a lifetime-bound handle.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the core referred to by this handle
    /// lives at least as long as `'a`.
    pub unsafe fn to_handle<'a>(&self) -> CoreHandle<'a> {
        CoreHandle {
            core: self.core.clone(),
            phantom: PhantomData,
        }
    }

    /// Embeds a future using this raw handle.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the core referred to by this handle
    /// lives at least as long as `'a`.
    pub unsafe fn embed<'a, T>(&'a self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        self.core.raw.embed(fut)
    }
}
impl awaiter_trait::UnsafeAwaiter for RawCoreHandle {
    unsafe fn unsafe_await<T>(&self, f: Pin<&mut dyn Future<Output = T>>) -> T {
        unsafe { self.embed(f) }
    }
}
awaiter_trait::autoimpl!(<> RawCoreHandle as UnsafeAwaiter);
impl awaiter_trait_02::UnsafeAwaiter for RawCoreHandle {
    unsafe fn unsafe_await<T>(&self, f: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        unsafe { self.embed(f) }
    }
}
awaiter_trait_02::autoimpl!(<> RawCoreHandle as UnsafeAwaiter);
