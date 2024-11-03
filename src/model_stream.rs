use crate::PrefixTrie;
use futures::Stream;
use std::future::Future;
use std::mem;
use std::pin::Pin;
use std::ptr::NonNull;
use std::task::{Context, Poll};
use tikv_client::{Error as TikvError, Transaction};

pub struct ModelStream<T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut,
    Fut: Future<Output = Result<T, TikvError>>,
{
    trie: &'static PrefixTrie,
    prefix: String,
    inner_stream:
        Pin<Box<dyn Stream<Item = Result<String, TikvError>>>>,
    txn: NonNull<Transaction>,
    load_fn: F,
}

impl<T, F, Fut> ModelStream<T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut + Unpin,
    Fut: Future<Output = Result<T, TikvError>>,
{
    pub fn new(
        prefix: String,
        txn: &mut Transaction,
        load_fn: F,
    ) -> Self {
        let trie = Box::new(PrefixTrie::new("ergokv:__trie"));
        let trie = Box::leak(trie);
        let txn_ptr = NonNull::from(txn);

        // SAFETY: txn_ptr is valid for 'a and we have exclusive access
        let inner_stream =
            unsafe {
                Box::pin(trie.find_by_prefix(
                    &mut *txn_ptr.as_ptr(),
                    &prefix,
                ))
            };

        Self {
            trie,
            inner_stream,
            prefix,
            txn: txn_ptr,
            load_fn,
        }
    }
}

// SAFETY: The raw pointer is valid for 'a and we ensure exclusive access
unsafe impl<T, F, Fut> Send for ModelStream<T, F, Fut>
where
    F: for<'a> Fn(&'a mut Transaction, String) -> Fut + Send,
    Fut: Future<Output = Result<T, TikvError>> + Send,
{
}

impl<T, F, Fut> Drop for ModelStream<T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut,
    Fut: Future<Output = Result<T, TikvError>>,
{
    fn drop(&mut self) {
        let example = unsafe {
            Box::from_raw(
                self.trie as *const PrefixTrie
                    as *mut PrefixTrie,
            )
        };
        mem::drop(example);
    }
}

impl<T, F, Fut> Stream for ModelStream<T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut + Unpin,
    Fut: Future<Output = Result<T, TikvError>> + Unpin,
{
    type Item = Result<T, TikvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.inner_stream.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(full_key))) => {
                match full_key.strip_prefix(&self.prefix) {
                    Some(key_str) => {
                        dbg!(key_str);
                        // SAFETY: txn_ptr is valid for 'a and we have exclusive access
                        let txn =
                            unsafe { &mut *self.txn.as_ptr() };
                        let mut fut = (self.load_fn)(
                            txn,
                            key_str.to_string(),
                        );
                        Pin::new(&mut fut).poll(cx).map(Some)
                    }
                    None => Poll::Ready(Some(Err(
                        TikvError::StringError(
                            "Invalid key format in trie".into(),
                        ),
                    ))),
                }
            }
            Poll::Ready(Some(Err(e))) => {
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
