use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;

pub struct MockNotify {
    inner: Arc<Inner>,
}

struct Inner {
    notified: AtomicBool,
}

impl MockNotify {
    pub fn new() -> Self {
        MockNotify {
            inner: Arc::new(Inner {
                notified: AtomicBool::new(false),
            }),
        }
    }

    pub fn with<F: FnOnce() -> R, R>(&self, _f: F) -> R {
        unimplemented!();
        // use futures::future::poll_fn;

        // self.clear();

        // let mut f = Some(f);

        // let res = tokio::spawn(poll_fn(move |cx| {
        //     Poll::Ready(f.take().unwrap()())
        // })).poll_future_notify(&self.inner, 0);

        // match res {
        //     Poll::Ready(v) => v,
        //     _ => unreachable!(),
        // }
    }

    pub fn clear(&self) {
        self.inner.notified.store(false, SeqCst);
    }

    pub fn is_notified(&self) -> bool {
        self.inner.notified.load(SeqCst)
    }
}

// impl Notify for Inner {
//     fn notify(&self, _: usize) {
//         self.notified.store(true, SeqCst);
//     }
// }
