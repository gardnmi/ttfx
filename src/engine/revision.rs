//! Unique identities for caches that must notice replacement as well as edits.

#[derive(Debug)]
pub(crate) struct Revision(pub(crate) u64);

impl Default for Revision {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let value = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if value == u64::MAX {
            std::process::abort();
        }
        Self(value)
    }
}

impl Clone for Revision {
    fn clone(&self) -> Self {
        Self::default()
    }
}
