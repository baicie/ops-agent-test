use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::{OpsCodexError, Result};

pub async fn retry_readonly<T, F, Fut>(
    cancellation: &CancellationToken,
    max_attempts: u32,
    mut op: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(error) if error.retryable() && attempt + 1 < max_attempts => {
                attempt += 1;
                let delay = Duration::from_millis(40 * 2u64.pow(attempt) + u64::from(attempt) * 13);
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(OpsCodexError::Cancelled),
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn http_status_error(service: &str, status: reqwest::StatusCode, body: &str) -> OpsCodexError {
    let class = match status.as_u16() {
        401 | 403 => crate::ConnectorClass::Auth,
        408 => crate::ConnectorClass::Timeout,
        429 => crate::ConnectorClass::RateLimit,
        400 | 404 | 422 => crate::ConnectorClass::InvalidQuery,
        _ if status.is_server_error() => crate::ConnectorClass::Unavailable,
        _ => crate::ConnectorClass::Unavailable,
    };
    OpsCodexError::connector(
        class,
        format!("{service} returned HTTP {status}: {}", truncate(body, 240)),
    )
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut end = max_chars.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    if value.len() <= max_chars {
        value.to_owned()
    } else {
        format!("{}…", &value[..end])
    }
}
