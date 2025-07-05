use crate::storage::queue::SendTask;
use std::time::Duration;
use tokio::time::sleep;

pub struct RetryPolicy;

impl RetryPolicy {
    pub async fn execute_with_retry<F, Fut>(task: &SendTask, mut operation: F) -> Result<(), String>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<(), String>>,
    {
        let mut retry_count = 0;
        loop {
            match operation().await {
                Ok(_) => return Ok(()),
                Err(e) if retry_count < task.max_retries => {
                    retry_count += 1;
                    let delay = Duration::from_secs(2u64.pow(retry_count.min(6)));
                    sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
} 