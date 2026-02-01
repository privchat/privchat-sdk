//! 文件发送队列 - 仅存需上传附件的任务，2～3 个消费者并发处理，不阻塞消息队列。

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::error::Result;
use crate::storage::queue::file_send_task::FileSendTask;

/// 文件发送队列（内存，支持多消费者 pop）
#[derive(Debug, Clone)]
pub struct FileSendQueue {
    inner: Arc<Mutex<VecDeque<FileSendTask>>>,
}

impl FileSendQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// 入队
    pub async fn push(&self, task: FileSendTask) -> Result<()> {
        let mut q = self.inner.lock().await;
        q.push_back(task);
        Ok(())
    }

    /// 出队（单条，供 worker 竞争）
    pub async fn pop(&self) -> Result<Option<FileSendTask>> {
        let mut q = self.inner.lock().await;
        Ok(q.pop_front())
    }

    /// 当前待处理数量
    pub async fn len(&self) -> usize {
        let q = self.inner.lock().await;
        q.len()
    }
}

impl Default for FileSendQueue {
    fn default() -> Self {
        Self::new()
    }
}
