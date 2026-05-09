use tokio_util::sync::CancellationToken;

pub fn build_cancel_token() -> CancellationToken {
    CancellationToken::new()
}
