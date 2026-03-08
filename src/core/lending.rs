use std::future::Future;

/// GAT: Connection lending -- guard borrows from pool, no Arc needed.
/// The Channel inside PoolEntry is tonic::Channel (internally Arc'd HTTP/2 --
/// that's tonic's concern, not ours). Our ConnectionGuard holds &'pool PoolEntry.
pub trait Lend {
    type Loan<'pool>
    where
        Self: 'pool;
    type Error;

    fn lend<'pool>(
        &'pool self,
        key: &str,
    ) -> impl Future<Output = Result<Self::Loan<'pool>, Self::Error>> + 'pool;
}
