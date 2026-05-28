//! Rayon thread pool shared by the parallel work-item processor
//! (feature `find_image_parallel`).
//!
//! Uses half the host CPU count so the pool doesn't oversubscribe when
//! it runs inside Tokio's blocking thread (which has its own pool).

use std::sync::OnceLock;

static FIND_IMAGE_POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

pub(super) fn get_thread_pool() -> &'static rayon::ThreadPool {
    FIND_IMAGE_POOL.get_or_init(|| {
        let num_threads = (rayon::current_num_threads() / 2).max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .expect("failed to create rayon thread pool")
    })
}
