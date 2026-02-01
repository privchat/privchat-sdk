//! 事件系统 - 处理账号间的消息和事件

use tokio::sync::mpsc;
use std::collections::HashMap;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub enum AccountEvent {
    /// 消息接收事件
    MessageReceived {
        account: String,
        from: u64,  // 发送者 user_id
        content: String,
        channel: u64,  // 频道ID
        message_id: u64,  // 服务器生成的 message_id
    },
    /// RPC 调用成功
    RpcSuccess {
        account: String,
        operation: String,
        result: String,
    },
    /// RPC 调用失败
    RpcError {
        account: String,
        operation: String,
        error: String,
    },
    /// 连接状态变化
    ConnectionStateChanged {
        account: String,
        connected: bool,
    },
    /// 消息发送成功
    MessageSent {
        account: String,
        to: u64,  // 接收者 user_id 或频道ID
        message_id: u64,  // 服务器生成的 message_id
        channel: u64,  // 频道ID
    },
    /// 消息撤回事件
    MessageRevoked {
        account: String,
        message_id: u64,  // 被撤回的消息ID
        channel_id: u64,  // 会话ID
        revoked_by: u64,  // 撤回者ID
    },
}

/// 事件总线 - 管理所有账号的事件
pub struct EventBus {
    sender: mpsc::UnboundedSender<AccountEvent>,
    receiver: mpsc::UnboundedReceiver<AccountEvent>,
    event_history: Vec<AccountEvent>,
    message_tracking: HashMap<String, Vec<String>>, // channel -> message_ids
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        
        Self {
            sender,
            receiver,
            event_history: Vec::new(),
            message_tracking: HashMap::new(),
        }
    }
    
    /// 获取事件发送器的克隆
    pub fn get_sender(&self) -> mpsc::UnboundedSender<AccountEvent> {
        self.sender.clone()
    }
    
    /// 发送事件
    pub fn send_event(&self, event: AccountEvent) -> Result<(), mpsc::error::SendError<AccountEvent>> {
        self.sender.send(event)
    }
    
    /// 处理事件 (非阻塞)
    pub async fn process_events(&mut self) -> usize {
        let mut processed = 0;
        
        while let Ok(event) = self.receiver.try_recv() {
            self.handle_event(event).await;
            processed += 1;
        }
        
        processed
    }
    
    /// 等待并处理单个事件
    pub async fn wait_for_event(&mut self) -> Option<AccountEvent> {
        match self.receiver.recv().await {
            Some(event) => {
                let event_copy = event.clone();
                self.handle_event(event).await;
                Some(event_copy)
            }
            None => None,
        }
    }
    
    /// 处理单个事件
    async fn handle_event(&mut self, event: AccountEvent) {
        match &event {
            AccountEvent::MessageReceived { account, from, content, channel, message_id } => {
                info!("📥 {} 收到消息: {} (from: {}) 频道: {} 内容: {}", account, message_id, from, channel, content);
            }
            AccountEvent::MessageSent { account, to, message_id, channel } => {
                info!("📤 {} 发送消息: {} 频道: {} ID: {}", account, to, channel, message_id);
                
                // 跟踪消息
                let channel_key = channel.to_string();
                let message_id_str = message_id.to_string();
                self.message_tracking
                    .entry(channel_key)
                    .or_insert_with(Vec::new)
                    .push(message_id_str);
            }
            AccountEvent::RpcSuccess { account, operation, result } => {
                info!("🔧 {} RPC成功: {} -> {}", account, operation, result);
            }
            AccountEvent::RpcError { account, operation, error } => {
                warn!("❌ {} RPC失败: {} -> {}", account, operation, error);
            }
            AccountEvent::ConnectionStateChanged { account, connected } => {
                if *connected {
                    info!("🟢 {} 已连接", account);
                } else {
                    warn!("🔴 {} 已断开", account);
                }
            }
            AccountEvent::MessageRevoked { account, message_id, channel_id, revoked_by } => {
                info!("🗑️ {} 收到撤回事件: message_id={}, channel_id={}, revoked_by={}", 
                      account, message_id, channel_id, revoked_by);
            }
        }
        
        // 保存到历史记录
        self.event_history.push(event);
    }
    
    /// 获取事件历史
    pub fn get_event_history(&self) -> &[AccountEvent] {
        &self.event_history
    }
    
    /// 获取特定账户的事件历史
    pub fn get_event_history_for_account(&self, account: &str) -> Vec<AccountEvent> {
        self.event_history.iter()
            .filter(|event| {
                match event {
                    AccountEvent::MessageReceived { account: acc, .. } => acc == account,
                    AccountEvent::RpcSuccess { account: acc, .. } => acc == account,
                    AccountEvent::RpcError { account: acc, .. } => acc == account,
                    AccountEvent::ConnectionStateChanged { account: acc, .. } => acc == account,
                    AccountEvent::MessageSent { account: acc, .. } => acc == account,
                    AccountEvent::MessageRevoked { account: acc, .. } => acc == account,
                }
            })
            .cloned()
            .collect()
    }
    
    /// 获取特定频道的消息数量
    pub fn get_message_count(&self, channel: u64) -> usize {
        let channel_key = channel.to_string();
        self.message_tracking
            .get(&channel_key)
            .map(|messages| messages.len())
            .unwrap_or(0)
    }
    
    /// 清理事件历史
    pub fn clear_history(&mut self) {
        self.event_history.clear();
        self.message_tracking.clear();
    }
    
    /// 等待特定类型的事件
    pub async fn wait_for_message_received(&mut self, account: &str, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        
        while start.elapsed() < timeout {
            if let Some(event) = self.wait_for_event().await {
                if let AccountEvent::MessageReceived { account: recv_account, .. } = event {
                    if recv_account == account {
                        return true;
                    }
                }
            }
        }
        
        false
    }
    
    /// 生成事件统计报告
    pub fn generate_event_report(&self) -> String {
        let mut report = String::new();
        
        let mut message_sent_count = 0;
        let mut message_received_count = 0;
        let mut rpc_success_count = 0;
        let mut rpc_error_count = 0;
        let mut connection_changes = 0;
        
        for event in &self.event_history {
            match event {
                AccountEvent::MessageSent { .. } => message_sent_count += 1,
                AccountEvent::MessageReceived { .. } => message_received_count += 1,
                AccountEvent::RpcSuccess { .. } => rpc_success_count += 1,
                AccountEvent::RpcError { .. } => rpc_error_count += 1,
                AccountEvent::ConnectionStateChanged { .. } => connection_changes += 1,
                AccountEvent::MessageRevoked { .. } => {},  // 撤回事件不计入统计
            }
        }
        
        report.push_str(&format!("📊 事件统计报告\n"));
        report.push_str(&format!("================\n"));
        report.push_str(&format!("📤 消息发送: {} 条\n", message_sent_count));
        report.push_str(&format!("📥 消息接收: {} 条\n", message_received_count));
        report.push_str(&format!("✅ RPC 成功: {} 次\n", rpc_success_count));
        report.push_str(&format!("❌ RPC 失败: {} 次\n", rpc_error_count));
        report.push_str(&format!("🔄 连接变化: {} 次\n", connection_changes));
        report.push_str(&format!("📈 总事件数: {} 个\n", self.event_history.len()));
        
        if !self.message_tracking.is_empty() {
            report.push_str("\n📋 频道消息统计:\n");
            for (channel, messages) in &self.message_tracking {
                report.push_str(&format!("   • {}: {} 条消息\n", channel, messages.len()));
            }
        }
        
        report
    }
}