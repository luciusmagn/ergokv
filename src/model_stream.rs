use futures::Stream;
use tikv_client::{Error as TikvError, Transaction};

use std::future::Future;
use std::pin::Pin;
use std::ptr::NonNull;
use std::task::{Context, Poll};

use crate::{trie::PrefixTrieStream, PrefixTrie};

pub struct ModelStream<'a, T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut,
    Fut: Future<Output = Result<T, TikvError>>,
{
    prefix: String,
    inner_stream: PrefixTrieStream<'a>,
    txn: NonNull<Transaction>,
    load_fn: F,
}

impl<'a, T, F, Fut> ModelStream<'a, T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut + Unpin,
    Fut: Future<Output = Result<T, TikvError>>,
{
    pub fn new(
        prefix: String,
        txn: &'a mut Transaction,
        load_fn: F,
    ) -> Self {
        let trie = PrefixTrie::new("ergokv:__trie");
        let second: &'a mut Transaction =
            unsafe { &mut *(txn as *mut _) };
        let inner_stream = trie.find_by_prefix(second, &prefix);

        let txn_ptr = NonNull::from(txn);

        println!("owo");
        Self {
            inner_stream,
            prefix,
            txn: txn_ptr,
            load_fn,
        }
    }
}

unsafe impl<'a, T, F, Fut> Send for ModelStream<'a, T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut + Send,
    Fut: Future<Output = Result<T, TikvError>> + Send,
{
}

impl<'a, T, F, Fut> Stream for ModelStream<'a, T, F, Fut>
where
    F: Fn(&'static mut Transaction, String) -> Fut + Unpin,
    Fut: Future<Output = Result<T, TikvError>> + Unpin,
{
    type Item = Result<T, TikvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match dbg!(Pin::new(&mut self.inner_stream).poll_next(cx))
        {
            Poll::Ready(Some(Ok(full_key))) => {
                match full_key.strip_prefix(&self.prefix) {
                    Some(key_str) => {
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
