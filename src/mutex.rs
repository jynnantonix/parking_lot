// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::fmt;
use std::marker::PhantomData;
use raw_mutex::RawMutex;

/// A mutual exclusion primitive useful for protecting shared data
///
/// This mutex will block threads waiting for the lock to become available. The
/// mutex can also be statically initialized or created via a `new`
/// constructor. Each mutex has a type parameter which represents the data that
/// it is protecting. The data can only be accessed through the RAII guards
/// returned from `lock` and `try_lock`, which guarantees that the data is only
/// ever accessed when the mutex is locked.
///
/// # Differences from the standard library `Mutex`
///
/// - No poisoning, the lock is released normally on panic.
/// - Only requires 1 byte of space, whereas the standard library boxes the
///   `Mutex` due to platform limitations.
/// - A `MutexGuard` can be sent to another thread and unlocked there.
/// - Can be statically constructed (requires the `const_fn` nightly feature).
/// - Does not require any drop glue when dropped.
/// - Inline fast path for the uncontended case.
/// - Efficient handling of micro-contention using adaptive spinning.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use parking_lot::Mutex;
/// use std::thread;
/// use std::sync::mpsc::channel;
///
/// const N: usize = 10;
///
/// // Spawn a few threads to increment a shared variable (non-atomically), and
/// // let the main thread know once all increments are done.
/// //
/// // Here we're using an Arc to share memory among threads, and the data inside
/// // the Arc is protected with a mutex.
/// let data = Arc::new(Mutex::new(0));
///
/// let (tx, rx) = channel();
/// for _ in 0..10 {
///     let (data, tx) = (data.clone(), tx.clone());
///     thread::spawn(move || {
///         // The shared state can only be accessed once the lock is held.
///         // Our non-atomic increment is safe because we're the only thread
///         // which can access the shared state when the lock is held.
///         let mut data = data.lock();
///         *data += 1;
///         if *data == N {
///             tx.send(()).unwrap();
///         }
///         // the lock is unlocked here when `data` goes out of scope.
///     });
/// }
///
/// rx.recv().unwrap();
/// ```
pub struct Mutex<T: ?Sized> {
    raw: RawMutex,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

/// An RAII implementation of a "scoped lock" of a mutex. When this structure is
/// dropped (falls out of scope), the lock will be unlocked.
///
/// The data protected by the mutex can be accessed through this guard via its
/// `Deref` and `DerefMut` implementations.
#[must_use]
pub struct MutexGuard<'a, T: ?Sized + 'a> {
    mutex: &'a Mutex<T>,
    marker: PhantomData<&'a mut T>,
}

impl<T> Mutex<T> {
    /// Creates a new mutex in an unlocked state ready for use.
    #[cfg(feature = "nightly")]
    #[inline]
    pub const fn new(val: T) -> Mutex<T> {
        Mutex {
            data: UnsafeCell::new(val),
            raw: RawMutex::new(),
        }
    }

    /// Creates a new mutex in an unlocked state ready for use.
    #[cfg(not(feature = "nightly"))]
    #[inline]
    pub fn new(val: T) -> Mutex<T> {
        Mutex {
            data: UnsafeCell::new(val),
            raw: RawMutex::new(),
        }
    }

    /// Consumes this mutex, returning the underlying data.
    #[inline]
    pub fn into_inner(self) -> T {
        unsafe { self.data.into_inner() }
    }
}

impl<T: ?Sized> Mutex<T> {
    /// Acquires a mutex, blocking the current thread until it is able to do so.
    ///
    /// This function will block the local thread until it is available to acquire
    /// the mutex. Upon returning, the thread is the only thread with the mutex
    /// held. An RAII guard is returned to allow scoped unlock of the lock. When
    /// the guard goes out of scope, the mutex will be unlocked.
    ///
    /// Attempts to lock a mutex in the thread which already holds the lock will
    /// result is a deadlock.
    #[inline]
    pub fn lock(&self) -> MutexGuard<T> {
        self.raw.lock();
        MutexGuard {
            mutex: self,
            marker: PhantomData,
        }
    }

    /// Attempts to acquire this lock.
    ///
    /// If the lock could not be acquired at this time, then `None` is returned.
    /// Otherwise, an RAII guard is returned. The lock will be unlocked when the
    /// guard is dropped.
    ///
    /// This function does not block.
    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<T>> {
        if self.raw.try_lock() {
            Some(MutexGuard {
                mutex: self,
                marker: PhantomData,
            })
        } else {
            None
        }
    }

    /// Returns a mutable reference to the underlying data.
    ///
    /// Since this call borrows the `Mutex` mutably, no actual locking needs to
    /// take place---the mutable borrow statically guarantees no locks exist.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data.get() }
    }
}

impl<T: ?Sized + Default> Default for Mutex<T> {
    #[inline]
    fn default() -> Mutex<T> {
        Mutex::new(Default::default())
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => write!(f, "Mutex {{ data: {:?} }}", &*guard),
            None => write!(f, "Mutex {{ <locked> }}"),
        }
    }
}

impl<'a, T: ?Sized + 'a> Deref for MutexGuard<'a, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<'a, T: ?Sized + 'a> DerefMut for MutexGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T: ?Sized + 'a> Drop for MutexGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.mutex.raw.unlock();
    }
}

// Helper function used by Condvar, not publicly exported
#[inline]
pub fn guard_lock<'a, T: ?Sized>(guard: &MutexGuard<'a, T>) -> &'a RawMutex {
    &guard.mutex.raw
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use {Mutex, Condvar};

    struct Packet<T>(Arc<(Mutex<T>, Condvar)>);

    #[derive(Eq, PartialEq, Debug)]
    struct NonCopy(i32);

    unsafe impl<T: Send> Send for Packet<T> {}
    unsafe impl<T> Sync for Packet<T> {}

    #[test]
    fn smoke() {
        let m = Mutex::new(());
        drop(m.lock());
        drop(m.lock());
    }

    #[test]
    fn lots_and_lots() {
        lazy_static! {
            static ref M: Mutex<()> = Mutex::new(());
        }
        static mut CNT: u32 = 0;
        const J: u32 = 1000;
        const K: u32 = 3;

        fn inc() {
            for _ in 0..J {
                unsafe {
                    let _g = M.lock();
                    CNT += 1;
                }
            }
        }

        let (tx, rx) = channel();
        for _ in 0..K {
            let tx2 = tx.clone();
            thread::spawn(move || {
                inc();
                tx2.send(()).unwrap();
            });
            let tx2 = tx.clone();
            thread::spawn(move || {
                inc();
                tx2.send(()).unwrap();
            });
        }

        drop(tx);
        for _ in 0..2 * K {
            rx.recv().unwrap();
        }
        assert_eq!(unsafe { CNT }, J * K * 2);
    }

    #[test]
    fn try_lock() {
        let m = Mutex::new(());
        *m.try_lock().unwrap() = ();
    }

    #[test]
    fn test_into_inner() {
        let m = Mutex::new(NonCopy(10));
        assert_eq!(m.into_inner(), NonCopy(10));
    }

    #[test]
    fn test_into_inner_drop() {
        struct Foo(Arc<AtomicUsize>);
        impl Drop for Foo {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let num_drops = Arc::new(AtomicUsize::new(0));
        let m = Mutex::new(Foo(num_drops.clone()));
        assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        {
            let _inner = m.into_inner();
            assert_eq!(num_drops.load(Ordering::SeqCst), 0);
        }
        assert_eq!(num_drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_get_mut() {
        let mut m = Mutex::new(NonCopy(10));
        *m.get_mut() = NonCopy(20);
        assert_eq!(m.into_inner(), NonCopy(20));
    }

    #[test]
    fn test_mutex_arc_condvar() {
        let packet = Packet(Arc::new((Mutex::new(false), Condvar::new())));
        let packet2 = Packet(packet.0.clone());
        let (tx, rx) = channel();
        let _t = thread::spawn(move || {
            // wait until parent gets in
            rx.recv().unwrap();
            let &(ref lock, ref cvar) = &*packet2.0;
            let mut lock = lock.lock();
            *lock = true;
            cvar.notify_one();
        });

        let &(ref lock, ref cvar) = &*packet.0;
        let mut lock = lock.lock();
        tx.send(()).unwrap();
        assert!(!*lock);
        while !*lock {
            cvar.wait(&mut lock);
        }
    }

    #[test]
    fn test_mutex_arc_nested() {
        // Tests nested mutexes and access
        // to underlying data.
        let arc = Arc::new(Mutex::new(1));
        let arc2 = Arc::new(Mutex::new(arc));
        let (tx, rx) = channel();
        let _t = thread::spawn(move || {
            let lock = arc2.lock();
            let lock2 = lock.lock();
            assert_eq!(*lock2, 1);
            tx.send(()).unwrap();
        });
        rx.recv().unwrap();
    }

    #[test]
    fn test_mutex_arc_access_in_unwind() {
        let arc = Arc::new(Mutex::new(1));
        let arc2 = arc.clone();
        let _ = thread::spawn(move || -> () {
                struct Unwinder {
                    i: Arc<Mutex<i32>>,
                }
                impl Drop for Unwinder {
                    fn drop(&mut self) {
                        *self.i.lock() += 1;
                    }
                }
                let _u = Unwinder { i: arc2 };
                panic!();
            })
            .join();
        let lock = arc.lock();
        assert_eq!(*lock, 2);
    }

    #[test]
    fn test_mutex_unsized() {
        let mutex: &Mutex<[i32]> = &Mutex::new([1, 2, 3]);
        {
            let b = &mut *mutex.lock();
            b[0] = 4;
            b[2] = 5;
        }
        let comp: &[i32] = &[4, 2, 5];
        assert_eq!(&*mutex.lock(), comp);
    }

    #[test]
    fn test_mutexguard_send() {
        fn send<T: Send>(_: T) {}

        let mutex = Mutex::new(());
        send(mutex.lock());
    }
}
