//! Task handle for managing async operations
//! 
//! This provides a way for FFI consumers to cancel or check the status
//! of long-running async operations.

use tokio::task::JoinHandle;
use tracing::debug;

/// A handle to a running async task
#[derive(uniffi::Object)]
pub struct TaskHandle {
    handle: JoinHandle<()>,
}

impl TaskHandle {
    /// Create a new task handle
    pub fn new(handle: JoinHandle<()>) -> Self {
        Self { handle }
    }
}

#[privchat_ffi_macros::export]
impl TaskHandle {
    /// Cancel the task
    /// 
    /// This will abort the underlying async operation.
    /// The task will stop as soon as possible.
    pub fn cancel(&self) {
        debug!("Cancelling task");
        self.handle.abort();
    }
    
    /// Check if the task has finished
    /// 
    /// Returns true if the task has completed (either successfully or by cancellation),
    /// false if it's still running.
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        // Auto-cancel on drop to prevent leaked tasks
        if !self.handle.is_finished() {
            debug!("Auto-cancelling task on drop");
            self.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_task_handle_cancel() {
        let handle = tokio::spawn(async {
            sleep(Duration::from_secs(10)).await;
        });
        
        let task_handle = TaskHandle::new(handle);
        assert!(!task_handle.is_finished());
        
        task_handle.cancel();
        
        // Give it a moment to cancel
        sleep(Duration::from_millis(10)).await;
        assert!(task_handle.is_finished());
    }
}
