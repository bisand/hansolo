//! Lowering the scheduling priority of the current thread.
//!
//! Hashing threads should give way to the UI and everything else the user runs.
//! Each OS spells that differently, and what we want is *per thread*, not the
//! whole process (the engine and UI share it).

/// Best effort; failures are ignored because mining at normal priority is still correct.
pub(crate) fn lower_current_thread() {
    #[cfg(target_vendor = "apple")]
    {
        // Lowest relative priority within the default QoS class. The UI runs
        // at user-interactive QoS and preempts these threads. Utility QoS, the
        // obvious choice, measured far worse on an M5 Pro with 18 threads:
        // 150–300 MH/s against ~520 MH/s for this.
        // SAFETY: plain FFI call affecting only the calling thread.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_DEFAULT, -15);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // On Linux, PRIO_PROCESS with a thread id sets that thread's nice value.
        // SAFETY: plain FFI calls affecting only the calling thread.
        unsafe {
            let tid = libc::gettid();
            libc::setpriority(libc::PRIO_PROCESS as _, tid as libc::id_t, 19);
        }
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_IDLE,
        };
        // SAFETY: GetCurrentThread returns a pseudo-handle valid for the calling
        // thread; SetThreadPriority only changes that thread.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_IDLE);
        }
    }

    // Other Unixes only offer process-wide nice; leave them alone.
}
