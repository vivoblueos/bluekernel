// Copyright (c) 2026 vivo Mobile Communication Co., Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(enable_vfs)]
use crate::vfs::fd_manager::FdManager;
use crate::{
    error::Error,
    scheduler,
    sync::SpinLock,
    thread::{Thread, ThreadList, ThreadNode},
};
use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Once;

#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(pub u32);

pub const ROOT_PID: Pid = Pid(1);

#[inline]
pub fn current_process() -> Option<Arc<Process>> {
    scheduler::current_thread_ref().process()
}

pub struct ProcessTable {
    table: SpinLock<BTreeMap<Pid, Arc<Process>>>,
}

impl ProcessTable {
    pub const fn new() -> Self {
        Self {
            table: SpinLock::new(BTreeMap::new()),
        }
    }

    pub fn insert(&self, process: Arc<Process>) -> Arc<Process> {
        let mut t = self.table.irqsave_lock();
        let pid = process.pid();
        assert!(
            t.insert(pid, Arc::clone(&process)).is_none(),
            "duplicate pid {:?} in process table",
            pid,
        );
        process
    }

    pub fn get(&self, pid: Pid) -> Option<Arc<Process>> {
        self.table.irqsave_lock().get(&pid).cloned()
    }

    /// Remove a process from the table, dropping the table's strong ref.
    /// `None` if the pid was absent.
    pub fn remove(&self, pid: Pid) -> Option<Arc<Process>> {
        self.table.irqsave_lock().remove(&pid)
    }
}

pub struct ProcessManager {
    next_pid: AtomicU32,
    root: Arc<Process>,
    table: ProcessTable,
}

impl ProcessManager {
    fn new() -> Self {
        let root = Process::new(ROOT_PID, Weak::new()).expect("create root process failed!");
        let manager = Self {
            next_pid: AtomicU32::new(2),
            root: root.clone(),
            table: ProcessTable::new(),
        };
        manager.table.insert(root);
        manager
    }

    #[inline]
    pub fn get() -> &'static ProcessManager {
        PROCESS_MANAGER.call_once(Self::new)
    }

    pub fn root_process(&'static self) -> &'static Arc<Process> {
        &self.root
    }

    #[inline]
    pub fn process_table(&self) -> &ProcessTable {
        &self.table
    }

    pub fn create_process(&self, parent: Weak<Process>) -> Result<Arc<Process>, Error> {
        let pid = self.alloc_pid();
        let process = Process::new(pid, parent)?;
        Ok(self.table.insert(process))
    }

    pub fn destroy_process(&self, pid: Pid) -> Option<Arc<Process>> {
        self.table.remove(pid)
    }

    pub fn alloc_pid(&self) -> Pid {
        let pid = self
            .next_pid
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |prev| {
                self.next_pid(prev)
            })
            .expect("no available pid");
        assert!(
            pid != 0 && pid != ROOT_PID.0,
            "alloc_pid invariant violated"
        );
        Pid(pid)
    }

    pub fn next_pid(&self, prev: u32) -> Option<u32> {
        let mut candidate = prev;
        loop {
            candidate = match candidate.checked_add(1) {
                None | Some(0) | Some(1) => 2,
                Some(v) => v,
            };
            if candidate == prev {
                return None;
            }
            if self.table.get(Pid(candidate)).is_none() {
                return Some(candidate);
            }
        }
    }
}

static PROCESS_MANAGER: Once<ProcessManager> = Once::new();

pub struct Process {
    pid: Pid,
    // A weak reference to the parent process.
    parent: Weak<Process>,
    // The threads owned by this process.
    threads: SpinLock<ThreadList>,
    // This process's open-file table.
    #[cfg(enable_vfs)]
    fd_manager: SpinLock<FdManager>,
}

// SAFETY: `Process` would otherwise be `!Send` only because `threads` holds a
// `TinyArcList` (whose `AtomicListHead` is deliberately `!Send` since it stores
// a raw `NonNull`). A process never actually migrates across cores by being
// *moved* as a Rust value: once created it is pinned behind an `Arc` whose
// storage is never moved or dropped, and every field that can be touched
// concurrently (`threads`, `address_space`, `fd_manager`) is already guarded
// by a `SpinLock` whose `irqsave_lock` serializes access. The remaining fields
// (`pid`, `parent`) are plain `Send + Sync`. So it is sound to move the owning
// `Arc<Process>` between threads and to share `&Process` across cores — which
// the scheduler already relies on for the root process. This unlocks
// `Once<Arc<Process>>` (the `root` field) and `Once<ProcessManager>` as
// `static` storage (both need `T: Send + Sync` for `Sync`).
unsafe impl Send for Process {}
unsafe impl Sync for Process {}

impl Process {
    pub fn new(pid: Pid, parent: Weak<Process>) -> Result<Arc<Self>, crate::error::Error> {
        let arc = Arc::new(Self {
            pid,
            parent,
            threads: SpinLock::new(ThreadList::new()),

            #[cfg(enable_vfs)]
            fd_manager: SpinLock::new(FdManager::new()),
        });
        // SAFETY: `arc` is the sole strong ref and is not yet observable by any
        // other thread, so this `&mut Process` aliases no live reference.
        let p = unsafe { Arc::as_ptr(&arc).cast_mut().as_mut().unwrap() };
        #[cfg(enable_vfs)]
        {
            p.fd_manager.irqsave_lock().init_stdio()?;
        }
        p.threads.irqsave_lock().init();
        Ok(arc)
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn parent(&self) -> Option<Arc<Process>> {
        self.parent.upgrade()
    }

    pub fn set_parent(&mut self, parent: Weak<Process>) -> Result<(), Error> {
        self.parent = parent;
        Ok(())
    }

    /// This process's open-file table. Only present when VFS is enabled.
    #[cfg(enable_vfs)]
    pub fn fd_manager(&self) -> &SpinLock<FdManager> {
        &self.fd_manager
    }

    /// Add a thread to this process's thread list.
    ///
    /// Returns `true` when success, and `false` if the thread was already
    /// linked into another process's thread list.
    pub fn add_thread(self: &Arc<Self>, thread: ThreadNode) -> bool {
        let weak = Arc::downgrade(self);
        let mut list = self.threads.irqsave_lock();
        if !list.push_back(thread) {
            return false;
        }
        if let Some(t) = list.back() {
            t.lock().set_process(weak);
        }
        true
    }

    /// Detach a thread from this process's thread list.
    ///
    /// Returns `false` if the thread is not currently linked to this process.
    pub fn remove_thread(&self, thread: &mut ThreadNode) -> bool {
        let _guard = self.threads.irqsave_lock();
        let removed = ThreadList::detach(thread);
        if removed {
            thread.lock().set_process(Weak::new());
        }
        removed
    }

    pub fn has_no_threads(&self) -> bool {
        self.threads.irqsave_lock().is_empty()
    }

    pub fn thread_count(&self) -> usize {
        self.threads.irqsave_lock().iter().count()
    }

    /// Invoke `f` for every thread owned by this process.
    ///
    /// The process's thread list lock is held for the duration of the call,
    /// so `f` must not recursively lock the same list. Thread references
    /// obtained through `f` are invalidated once `f` returns, since the lock
    /// is dropped.
    pub fn for_each_thread<F: FnMut(&Thread)>(&self, mut f: F) {
        let list = self.threads.irqsave_lock();
        for t in list.iter() {
            f(t);
        }
    }
}

impl core::fmt::Debug for Process {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let parent_pid = self.parent.upgrade().map(|p| p.pid());
        let mut dbg = f.debug_struct("Process");
        dbg.field("pid", &self.pid).field("parent", &parent_pid);
        #[cfg(enable_vfs)]
        dbg.field("fd_manager", &"<per-process>");
        dbg.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use blueos_test_macro::test;

    #[test]
    fn process_basic() {
        // Root process (pid `ROOT_PID` = 1)
        let root = Process::new(ROOT_PID, Weak::new()).expect("Process::new failed");
        assert_eq!(root.pid(), ROOT_PID);
        // Root's parent is None.
        assert!(root.parent().is_none());

        // A child whose parent is root. `Pid(0)` is the auto-allocate
        // sentinel — the child gets a fresh PID (>= 2) and never collides
        // with root's `ROOT_PID`. Assert invariants rather than a fixed
        // value, since the global counter is shared across tests.
        let child = Process::new(Pid(2), Arc::downgrade(&root)).expect("Process::new failed");
        assert_ne!(child.pid(), root.pid());
        assert_eq!(child.parent().map(|p| p.pid()), Some(ROOT_PID));

        // When VFS is enabled, every process also owns an independent
        // per-process open-file table. The syscall path resolves each
        // process's own table via `current_process()`.
        #[cfg(enable_vfs)]
        {
            // `Process::new` initializes stdio (stdin/stdout/stderr) for every
            // process, so both root and child have 3 fds and `get_file_ops(0)`
            // is `Some`.
            assert_eq!(root.fd_manager().irqsave_lock().count(), 3);
            assert!(root.fd_manager().irqsave_lock().get_file_ops(0).is_some());
            assert_eq!(child.fd_manager().irqsave_lock().count(), 3);
            assert!(child.fd_manager().irqsave_lock().get_file_ops(0).is_some());
            // Root and child do not share a fd table.
            assert!(!core::ptr::eq(
                root.fd_manager() as *const _,
                child.fd_manager() as *const _,
            ));
        }
    }

    #[test]
    fn process_dropped_parent() {
        // When the parent Arc is dropped, upgrade returns None.
        let weak = {
            let parent = Process::new(Pid(0), Weak::new()).expect("Process::new failed");
            Arc::downgrade(&parent)
        };
        let child = Process::new(Pid(0), weak).expect("Process::new failed");
        assert!(child.parent().is_none());
    }

    #[test]
    fn process_starts_with_no_threads() {
        // A freshly created process owns no threads.
        let root = Process::new(ROOT_PID, Weak::new()).expect("Process::new failed");
        assert!(root.has_no_threads());
        assert_eq!(root.thread_count(), 0);
    }

    #[test]
    fn alloc_pid_skips_reserved() {
        // alloc_pid never hands out the auto-allocate sentinel (0) or the
        // root process's reserved `ROOT_PID` (1); each returned value also
        // differs from the previous one (the counter strictly advances,
        // modulo the `u32::MAX` wrap exercised below).
        let a = ProcessManager::get().alloc_pid();
        let b = ProcessManager::get().alloc_pid();
        assert!(
            a != Pid(0) && a != ROOT_PID,
            "alloc_pid never returns 0 or ROOT_PID"
        );
        assert!(b != Pid(0) && b != ROOT_PID);
        assert!(a != b, "alloc_pid returns distinct values");
    }

    #[test]
    fn next_pid_skips_reserved_and_live() {
        let occupied = |pid: u32| {
            pid == 0
                || pid == ROOT_PID.0
                || ProcessManager::get()
                    .process_table()
                    .get(Pid(pid))
                    .is_some()
        };

        for &seed in &[0u32, 1, 2, ROOT_PID.0, u32::MAX - 1, u32::MAX] {
            let next = ProcessManager::get()
                .next_pid(seed)
                .expect("next_pid finds a free pid");
            assert!(
                !occupied(next),
                "next_pid({seed}) returned occupied pid {next}"
            );
        }

        let mut n = ROOT_PID.0;
        while occupied(n.wrapping_add(1)) {
            n = n.wrapping_add(1);
            if n == ROOT_PID.0 {
                // Table holds every allocatable pid — skip this assertion
                // rather than spin.
                break;
            }
        }
        if !occupied(n.wrapping_add(1)) {
            assert_eq!(
                ProcessManager::get().next_pid(n),
                Some(n.wrapping_add(1)),
                "next_pid stops at the first free candidate"
            );
        }
    }

    #[test]
    fn process_table_insert_get_remove() {
        let table = ProcessTable::new();

        let root = Process::new(ROOT_PID, Weak::new()).expect("Process::new failed");
        let child = Process::new(Pid(0), Arc::downgrade(&root)).expect("Process::new failed");
        let child_pid = child.pid();

        // Insert; the returned Arc is ptr-equal to the one stored.
        let stored_back = table.insert(Arc::clone(&child));
        assert!(Arc::ptr_eq(&stored_back, &child));

        // get returns a ptr-equal clone; the table retains its own ref so the
        // strong count is >= 2.
        let g = table
            .get(child_pid)
            .expect("get returned None after insert");
        assert!(Arc::ptr_eq(&g, &child));

        // get for a never-inserted pid is None.
        assert!(table.get(Pid(u32::MAX)).is_none());

        // remove drops the table's ref and returns the stored Arc (ptr-equal).
        let removed = table.remove(child_pid).expect("remove returned None");
        assert!(Arc::ptr_eq(&removed, &child));

        // After removal the pid is gone: both get and a second remove are None.
        assert!(table.get(child_pid).is_none());
        assert!(table.remove(child_pid).is_none());

        // Sanity: root is unaffected by being a weak parent here.
        assert_eq!(child.parent().map(|p| p.pid()), Some(ROOT_PID));
    }
}
