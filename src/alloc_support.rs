use core::alloc::Layout;

use crate::*;
use alloc::{boxed::Box, sync::Arc, vec::Vec};

#[repr(transparent)]
pub struct Stack {
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

pub struct Core {
    raw: RawCore,
}
impl Core {
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
    pub struct Coroutine<U,T>{
        #[pin]
        raw: RawCoroutine<(U,Arc<Core>),T>,
        keepalive: Arc<Core>
    }
}
impl<U: FnOnce(CoreHandle<'_>) -> T, T> Coroutine<U, T> {
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
pub struct CoreHandle<'a> {
    core: Arc<Core>,
    phantom: PhantomData<&'a ()>,
}
impl<'a> CoreHandle<'a> {
    pub fn embed<T>(&self, mut fut: Pin<&mut (dyn Future<Output = T> + '_)>) -> T {
        self.core.raw.embed(fut)
    }
}
