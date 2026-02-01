//! RPC 客户端封装
//!
//! 本模块提供所有 RPC 接口的类型安全封装，使用 privchat-protocol 定义的请求/响应类型

use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use crate::error::{PrivchatSDKError, Result};
use crate::client::PrivchatClient;
use crate::rate_limiter::{RpcRequestKey, RpcRateLimitError};
use tracing::{info, warn};

// 导入协议层的 RPC 类型
use privchat_protocol::rpc::{
    // Account 模块
    account::{
        privacy::{AccountPrivacyGetRequest, AccountPrivacyGetResponse, AccountPrivacyUpdateRequest, AccountPrivacyUpdateResponse},
        user::{AccountUserDetailRequest, AccountUserDetailResponse, AccountUserUpdateRequest, AccountUserUpdateResponse, AccountUserShareCardRequest, AccountUserShareCardResponse},
        search::*,
    },
    // Contact 模块
    contact::*,
    // Channel 模块
    channel::*,
    // File 模块
    file::upload::{FileRequestUploadTokenRequest, FileRequestUploadTokenResponse, FileUploadCallbackRequest, FileUploadCallbackResponse},
    // Group 模块
    group::{
        member::*,
        member_mute::{GroupMemberMuteRequest, GroupMemberUnmuteRequest, GroupMemberMuteResponse, GroupMemberUnmuteResponse},
        qrcode::*,
        role_set::*,
        settings::*,
        transfer::*,
        approval::{GroupApprovalListRequest, GroupApprovalListResponse, GroupApprovalHandleRequest, GroupApprovalHandleResponse},
        group::*,
    },
    // Message 模块
    message::*,
};

// ========== Device 模块类型定义 ==========

/// 设备列表请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceListRequest {
    /// 用户ID
    pub user_id: u64,
    /// 当前设备ID（可选，用于标记当前设备）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

/// 设备列表响应项（服务器返回的格式）
/// 
/// 注意：时间字段统一使用 UNIX 时间戳（毫秒，UTC）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceListItem {
    pub device_id: String,
    pub device_name: String,
    pub device_model: String,
    pub app_id: String,
    #[serde(rename = "device_type")]
    pub device_type: String, // 服务器返回的是字符串，如 "ios", "android" 等
    /// 最后活跃时间（UNIX 时间戳，毫秒，UTC）
    #[serde(deserialize_with = "deserialize_timestamp")]
    pub last_active_at: u64,
    /// 创建时间（UNIX 时间戳，毫秒，UTC）
    #[serde(deserialize_with = "deserialize_timestamp")]
    pub created_at: u64,
    pub ip_address: String,
    pub is_current: bool,
}

/// 反序列化时间戳（支持 ISO 8601 字符串或数字时间戳）
fn deserialize_timestamp<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct TimestampVisitor;

    impl<'de> Visitor<'de> for TimestampVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a timestamp (u64 milliseconds) or ISO 8601 string")
        }

        fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value < 0 {
                return Err(E::custom(format!("timestamp cannot be negative: {}", value)));
            }
            Ok(value as u64)
        }

        fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
        where
            E: de::Error,
        {
            // 尝试解析 ISO 8601 字符串
            match chrono::DateTime::parse_from_rfc3339(value) {
                Ok(dt) => Ok(dt.timestamp_millis() as u64),
                Err(_) => {
                    // 如果解析失败，尝试作为数字字符串解析
                    value.parse::<u64>()
                        .map_err(|_| E::custom(format!("invalid timestamp format: {}", value)))
                }
            }
        }
    }

    deserializer.deserialize_any(TimestampVisitor)
}

/// 设备列表响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceListItem>,
    pub total: usize,
}

// ========== RPC 客户端扩展 trait ==========

/// RPC 客户端扩展 trait，为 PrivchatClient 添加类型安全的 RPC 方法
#[allow(async_fn_in_trait)]
pub trait RpcClientExt {
    // ========== Message 模块 ==========
    
    /// 撤回消息
    async fn message_revoke(&mut self, request: MessageRevokeRequest) -> Result<MessageRevokeResponse>;
    
    /// 获取消息历史
    async fn message_history_get(&mut self, request: MessageHistoryGetRequest) -> Result<MessageHistoryResponse>;
    
    /// 添加 Reaction
    async fn message_reaction_add(&mut self, request: MessageReactionAddRequest) -> Result<MessageReactionAddResponse>;
    
    /// 移除 Reaction
    async fn message_reaction_remove(&mut self, request: MessageReactionRemoveRequest) -> Result<MessageReactionRemoveResponse>;
    
    /// 获取 Reaction 列表
    async fn message_reaction_list(&mut self, request: MessageReactionListRequest) -> Result<MessageReactionListResponse>;
    
    /// 获取 Reaction 统计
    async fn message_reaction_stats(&mut self, request: MessageReactionStatsRequest) -> Result<MessageReactionStatsResponse>;
    
    // ========== Contact 模块 ==========
    
    /// 申请添加好友
    async fn contact_friend_apply(&mut self, request: FriendApplyRequest) -> Result<FriendApplyResponse>;
    
    /// 接受好友申请
    async fn contact_friend_accept(&mut self, request: FriendAcceptRequest) -> Result<FriendAcceptResponse>;
    
    /// 拒绝好友申请
    async fn contact_friend_reject(&mut self, request: FriendRejectRequest) -> Result<FriendRejectResponse>;
    
    /// 移除好友
    async fn contact_friend_remove(&mut self, request: FriendRemoveRequest) -> Result<FriendRemoveResponse>;
    
    /// 检查好友关系
    async fn contact_friend_check(&mut self, request: FriendCheckRequest) -> Result<FriendCheckResponse>;
    
    /// 获取待处理好友申请
    async fn contact_friend_pending(&mut self, request: FriendPendingRequest) -> Result<FriendPendingResponse>;
    
    /// 添加到黑名单
    async fn contact_blacklist_add(&mut self, request: BlacklistAddRequest) -> Result<BlacklistAddResponse>;
    
    /// 从黑名单移除
    async fn contact_blacklist_remove(&mut self, request: BlacklistRemoveRequest) -> Result<BlacklistRemoveResponse>;
    
    /// 检查黑名单状态
    async fn contact_blacklist_check(&mut self, request: BlacklistCheckRequest) -> Result<BlacklistCheckResponse>;
    
    /// 获取黑名单列表
    async fn contact_blacklist_list(&mut self, request: BlacklistListRequest) -> Result<BlacklistListResponse>;
    
    // ========== Group 模块 ==========
    
    /// 创建群组
    async fn group_create(&mut self, request: GroupCreateRequest) -> Result<GroupCreateResponse>;
    
    /// 获取群组信息
    async fn group_info(&mut self, request: GroupInfoRequest) -> Result<GroupInfoResponse>;
    
    /// 设置成员角色
    async fn group_role_set(&mut self, request: GroupRoleSetRequest) -> Result<GroupRoleSetResponse>;
    
    /// 转让群主
    async fn group_transfer_owner(&mut self, request: GroupTransferOwnerRequest) -> Result<GroupTransferOwnerResponse>;
    
    /// 添加群组成员
    async fn group_member_add(&mut self, request: GroupMemberAddRequest) -> Result<GroupMemberAddResponse>;
    
    /// 移除群组成员
    async fn group_member_remove(&mut self, request: GroupMemberRemoveRequest) -> Result<GroupMemberRemoveResponse>;
    
    /// 离开群组
    async fn group_member_leave(&mut self, request: GroupMemberLeaveRequest) -> Result<GroupMemberLeaveResponse>;
    
    /// 获取群组成员列表
    async fn group_member_list(&mut self, request: GroupMemberListRequest) -> Result<GroupMemberListResponse>;
    
    /// 禁言成员
    async fn group_member_mute(&mut self, request: GroupMemberMuteRequest) -> Result<GroupMemberMuteResponse>;
    
    /// 解除禁言
    async fn group_member_unmute(&mut self, request: GroupMemberUnmuteRequest) -> Result<GroupMemberUnmuteResponse>;
    
    /// 更新群组设置
    async fn group_settings_update(&mut self, request: GroupSettingsUpdateRequest) -> Result<GroupSettingsUpdateResponse>;
    
    /// 获取群组设置
    async fn group_settings_get(&mut self, request: GroupSettingsGetRequest) -> Result<GroupSettingsGetResponse>;
    
    /// 生成群组二维码
    async fn group_qrcode_generate(&mut self, request: GroupQRCodeGenerateRequest) -> Result<GroupQRCodeGenerateResponse>;
    
    /// 通过二维码加入群组
    async fn group_qrcode_join(&mut self, request: GroupQRCodeJoinRequest) -> Result<GroupQRCodeJoinResponse>;
    
    /// 获取群组申请列表
    async fn group_approval_list(&mut self, request: GroupApprovalListRequest) -> Result<GroupApprovalListResponse>;
    
    /// 处理群组申请（批准或拒绝）
    async fn group_approval_handle(&mut self, request: GroupApprovalHandleRequest) -> Result<GroupApprovalHandleResponse>;
    
    // ========== Account 模块 ==========
    
    /// 搜索账号
    async fn account_search_query(&mut self, request: AccountSearchQueryRequest) -> Result<AccountSearchResponse>;
    
    /// 通过二维码搜索账号
    async fn account_search_by_qrcode(&mut self, request: AccountSearchByQRCodeRequest) -> Result<AccountSearchResponse>;
    
    /// 获取账号隐私设置
    async fn account_privacy_get(&mut self, request: AccountPrivacyGetRequest) -> Result<AccountPrivacyGetResponse>;
    
    /// 更新账号隐私设置
    async fn account_privacy_update(&mut self, request: AccountPrivacyUpdateRequest) -> Result<AccountPrivacyUpdateResponse>;
    
    /// 获取用户详情
    async fn account_user_detail(&mut self, request: AccountUserDetailRequest) -> Result<AccountUserDetailResponse>;
    
    /// 更新用户信息
    async fn account_user_update(&mut self, request: AccountUserUpdateRequest) -> Result<AccountUserUpdateResponse>;
    
    /// 分享用户卡片
    async fn account_user_share_card(&mut self, request: AccountUserShareCardRequest) -> Result<AccountUserShareCardResponse>;
    
    // ========== Channel 模块 ==========
    
    /// 置顶/取消置顶会话
    async fn channel_pin(&mut self, request: ChannelPinRequest) -> Result<ChannelPinResponse>;
    
    // ========== File 模块 ==========
    
    /// 请求文件上传 Token
    async fn file_request_upload_token(&mut self, request: FileRequestUploadTokenRequest) -> Result<FileRequestUploadTokenResponse>;
    
    /// 文件上传回调
    async fn file_upload_callback(&mut self, request: FileUploadCallbackRequest) -> Result<FileUploadCallbackResponse>;
    
    // ========== Presence 模块 ==========
    
    /// 订阅用户在线状态
    async fn subscribe_presence(&mut self, request: privchat_protocol::presence::SubscribePresenceRequest) -> Result<privchat_protocol::presence::SubscribePresenceResponse>;
    
    /// 取消订阅用户在线状态
    async fn unsubscribe_presence(&mut self, request: privchat_protocol::presence::UnsubscribePresenceRequest) -> Result<privchat_protocol::presence::UnsubscribePresenceResponse>;
    
    /// 批量查询用户在线状态
    async fn get_online_status(&mut self, request: privchat_protocol::presence::GetOnlineStatusRequest) -> Result<privchat_protocol::presence::GetOnlineStatusResponse>;
    
    /// 发送输入状态通知
    async fn typing_indicator(&mut self, request: privchat_protocol::presence::TypingIndicatorRequest) -> Result<privchat_protocol::presence::TypingIndicatorResponse>;
    
    // ========== Device 模块 ==========
    
    /// 获取设备列表
    async fn device_list(&mut self, request: DeviceListRequest) -> Result<DeviceListResponse>;
    
    /// 更新设备推送状态
    async fn device_push_update(&mut self, request: privchat_protocol::rpc::device::DevicePushUpdateRequest) -> Result<privchat_protocol::rpc::device::DevicePushUpdateResponse>;
    
    /// 获取设备推送状态
    async fn device_push_status(&mut self, request: privchat_protocol::rpc::device::DevicePushStatusRequest) -> Result<privchat_protocol::rpc::device::DevicePushStatusResponse>;
}

// ========== 实现 RpcClientExt trait ==========

impl RpcClientExt for PrivchatClient {
    // ========== Message 模块 ==========
    
    async fn message_revoke(&mut self, request: MessageRevokeRequest) -> Result<MessageRevokeResponse> {
        self.call_rpc_typed("message/revoke", request).await
    }
    
    async fn message_history_get(&mut self, request: MessageHistoryGetRequest) -> Result<MessageHistoryResponse> {
        self.call_rpc_typed("message/history/get", request).await
    }
    
    async fn message_reaction_add(&mut self, request: MessageReactionAddRequest) -> Result<MessageReactionAddResponse> {
        self.call_rpc_typed("message/reaction/add", request).await
    }
    
    async fn message_reaction_remove(&mut self, request: MessageReactionRemoveRequest) -> Result<MessageReactionRemoveResponse> {
        self.call_rpc_typed("message/reaction/remove", request).await
    }
    
    async fn message_reaction_list(&mut self, request: MessageReactionListRequest) -> Result<MessageReactionListResponse> {
        self.call_rpc_typed("message/reaction/list", request).await
    }
    
    async fn message_reaction_stats(&mut self, request: MessageReactionStatsRequest) -> Result<MessageReactionStatsResponse> {
        self.call_rpc_typed("message/reaction/stats", request).await
    }
    
    // ========== Contact 模块 ==========
    
    async fn contact_friend_apply(&mut self, request: FriendApplyRequest) -> Result<FriendApplyResponse> {
        self.call_rpc_typed("contact/friend/apply", request).await
    }
    
    async fn contact_friend_accept(&mut self, request: FriendAcceptRequest) -> Result<FriendAcceptResponse> {
        self.call_rpc_typed("contact/friend/accept", request).await
    }
    
    async fn contact_friend_reject(&mut self, request: FriendRejectRequest) -> Result<FriendRejectResponse> {
        self.call_rpc_typed("contact/friend/reject", request).await
    }
    
    async fn contact_friend_remove(&mut self, request: FriendRemoveRequest) -> Result<FriendRemoveResponse> {
        self.call_rpc_typed("contact/friend/remove", request).await
    }
    
    async fn contact_friend_check(&mut self, request: FriendCheckRequest) -> Result<FriendCheckResponse> {
        self.call_rpc_typed("contact/friend/check", request).await
    }
    
    async fn contact_friend_pending(&mut self, request: FriendPendingRequest) -> Result<FriendPendingResponse> {
        self.call_rpc_typed("contact/friend/pending", request).await
    }
    
    async fn contact_blacklist_add(&mut self, request: BlacklistAddRequest) -> Result<BlacklistAddResponse> {
        self.call_rpc_typed("contact/blacklist/add", request).await
    }
    
    async fn contact_blacklist_remove(&mut self, request: BlacklistRemoveRequest) -> Result<BlacklistRemoveResponse> {
        self.call_rpc_typed("contact/blacklist/remove", request).await
    }
    
    async fn contact_blacklist_check(&mut self, request: BlacklistCheckRequest) -> Result<BlacklistCheckResponse> {
        self.call_rpc_typed("contact/blacklist/check", request).await
    }
    
    async fn contact_blacklist_list(&mut self, request: BlacklistListRequest) -> Result<BlacklistListResponse> {
        self.call_rpc_typed("contact/blacklist/list", request).await
    }
    
    // ========== Group 模块 ==========
    
    async fn group_create(&mut self, request: GroupCreateRequest) -> Result<GroupCreateResponse> {
        self.call_rpc_typed("group/group/create", request).await
    }
    
    async fn group_info(&mut self, request: GroupInfoRequest) -> Result<GroupInfoResponse> {
        self.call_rpc_typed("group/group/info", request).await
    }
    
    async fn group_role_set(&mut self, request: GroupRoleSetRequest) -> Result<GroupRoleSetResponse> {
        self.call_rpc_typed("group/role/set", request).await
    }
    
    async fn group_transfer_owner(&mut self, request: GroupTransferOwnerRequest) -> Result<GroupTransferOwnerResponse> {
        self.call_rpc_typed("group/role/transfer_owner", request).await
    }
    
    async fn group_member_add(&mut self, request: GroupMemberAddRequest) -> Result<GroupMemberAddResponse> {
        self.call_rpc_typed("group/member/add", request).await
    }
    
    async fn group_member_remove(&mut self, request: GroupMemberRemoveRequest) -> Result<GroupMemberRemoveResponse> {
        self.call_rpc_typed("group/member/remove", request).await
    }
    
    async fn group_member_leave(&mut self, request: GroupMemberLeaveRequest) -> Result<GroupMemberLeaveResponse> {
        self.call_rpc_typed("group/member/leave", request).await
    }
    
    async fn group_member_list(&mut self, request: GroupMemberListRequest) -> Result<GroupMemberListResponse> {
        self.call_rpc_typed("group/member/list", request).await
    }
    
    async fn group_member_mute(&mut self, request: GroupMemberMuteRequest) -> Result<GroupMemberMuteResponse> {
        self.call_rpc_typed("group/member/mute", request).await
    }
    
    async fn group_member_unmute(&mut self, request: GroupMemberUnmuteRequest) -> Result<GroupMemberUnmuteResponse> {
        self.call_rpc_typed("group/member/unmute", request).await
    }
    
    async fn group_settings_update(&mut self, request: GroupSettingsUpdateRequest) -> Result<GroupSettingsUpdateResponse> {
        self.call_rpc_typed("group/settings/update", request).await
    }
    
    async fn group_settings_get(&mut self, request: GroupSettingsGetRequest) -> Result<GroupSettingsGetResponse> {
        self.call_rpc_typed("group/settings/get", request).await
    }
    
    async fn group_qrcode_generate(&mut self, request: GroupQRCodeGenerateRequest) -> Result<GroupQRCodeGenerateResponse> {
        self.call_rpc_typed("group/qrcode/generate", request).await
    }
    
    async fn group_qrcode_join(&mut self, request: GroupQRCodeJoinRequest) -> Result<GroupQRCodeJoinResponse> {
        self.call_rpc_typed("group/qrcode/join", request).await
    }
    
    async fn group_approval_list(&mut self, request: GroupApprovalListRequest) -> Result<GroupApprovalListResponse> {
        self.call_rpc_typed("group/approval/list", request).await
    }
    
    async fn group_approval_handle(&mut self, request: GroupApprovalHandleRequest) -> Result<GroupApprovalHandleResponse> {
        self.call_rpc_typed("group/approval/handle", request).await
    }
    
    // ========== Account 模块 ==========
    
    async fn account_search_query(&mut self, request: AccountSearchQueryRequest) -> Result<AccountSearchResponse> {
        self.call_rpc_typed("account/search/query", request).await
    }
    
    async fn account_search_by_qrcode(&mut self, request: AccountSearchByQRCodeRequest) -> Result<AccountSearchResponse> {
        self.call_rpc_typed("account/search/by_qrcode", request).await
    }
    
    async fn account_privacy_get(&mut self, request: AccountPrivacyGetRequest) -> Result<AccountPrivacyGetResponse> {
        self.call_rpc_typed("account/privacy/get", request).await
    }
    
    async fn account_privacy_update(&mut self, request: AccountPrivacyUpdateRequest) -> Result<AccountPrivacyUpdateResponse> {
        self.call_rpc_typed("account/privacy/update", request).await
    }
    
    async fn account_user_detail(&mut self, request: AccountUserDetailRequest) -> Result<AccountUserDetailResponse> {
        self.call_rpc_typed("account/user/detail", request).await
    }
    
    async fn account_user_update(&mut self, request: AccountUserUpdateRequest) -> Result<AccountUserUpdateResponse> {
        self.call_rpc_typed("account/user/update", request).await
    }
    
    async fn account_user_share_card(&mut self, request: AccountUserShareCardRequest) -> Result<AccountUserShareCardResponse> {
        self.call_rpc_typed("account/user/share_card", request).await
    }
    
    // ========== Channel 模块 ==========
    
    async fn channel_pin(&mut self, request: ChannelPinRequest) -> Result<ChannelPinResponse> {
        self.call_rpc_typed("channel/pin", request).await
    }
    
    // ========== File 模块 ==========
    
    async fn file_request_upload_token(&mut self, request: FileRequestUploadTokenRequest) -> Result<FileRequestUploadTokenResponse> {
        self.call_rpc_typed("file/request_upload_token", request).await
    }
    
    async fn file_upload_callback(&mut self, request: FileUploadCallbackRequest) -> Result<FileUploadCallbackResponse> {
        self.call_rpc_typed("file/upload_callback", request).await
    }
    
    // ========== Presence 模块 ==========
    
    async fn subscribe_presence(&mut self, request: privchat_protocol::presence::SubscribePresenceRequest) -> Result<privchat_protocol::presence::SubscribePresenceResponse> {
        self.call_rpc_typed(privchat_protocol::rpc::routes::presence::SUBSCRIBE, request).await
    }
    
    async fn unsubscribe_presence(&mut self, request: privchat_protocol::presence::UnsubscribePresenceRequest) -> Result<privchat_protocol::presence::UnsubscribePresenceResponse> {
        self.call_rpc_typed(privchat_protocol::rpc::routes::presence::UNSUBSCRIBE, request).await
    }
    
    async fn get_online_status(&mut self, request: privchat_protocol::presence::GetOnlineStatusRequest) -> Result<privchat_protocol::presence::GetOnlineStatusResponse> {
        self.call_rpc_typed("presence/status/get", request).await
    }
    
    async fn typing_indicator(&mut self, request: privchat_protocol::presence::TypingIndicatorRequest) -> Result<privchat_protocol::presence::TypingIndicatorResponse> {
        self.call_rpc_typed(privchat_protocol::rpc::routes::presence::TYPING, request).await
    }
    
    // ========== Device 模块 ==========
    
    async fn device_list(&mut self, request: DeviceListRequest) -> Result<DeviceListResponse> {
        self.call_rpc_typed("device/list", request).await
    }
    
    async fn device_push_update(&mut self, request: privchat_protocol::rpc::device::DevicePushUpdateRequest) -> Result<privchat_protocol::rpc::device::DevicePushUpdateResponse> {
        use privchat_protocol::rpc::routes;
        self.call_rpc_typed(routes::device::PUSH_UPDATE, request).await
    }
    
    async fn device_push_status(&mut self, request: privchat_protocol::rpc::device::DevicePushStatusRequest) -> Result<privchat_protocol::rpc::device::DevicePushStatusResponse> {
        use privchat_protocol::rpc::routes;
        self.call_rpc_typed(routes::device::PUSH_STATUS, request).await
    }
}

// ========== PrivchatClient 扩展方法 ==========

impl PrivchatClient {
    /// 类型安全的 RPC 调用方法
    ///
    /// # 参数
    /// - `route`: RPC 路由路径
    /// - `request`: 请求对象（实现了 Serialize）
    ///
    /// # 返回
    /// - 成功返回响应对象（实现了 DeserializeOwned）
    pub async fn call_rpc_typed<Req, Res>(&mut self, route: &str, request: Req) -> Result<Res>
    where
        Req: serde::Serialize,
        Res: DeserializeOwned,
    {
        // 序列化请求
        let params = serde_json::to_value(&request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("序列化请求失败: {}", e)))?;
        
        // 🔥 检查 RPC 限流和去重（如果限流器已设置）
        let limiter_opt = self.get_rpc_rate_limiter().cloned();
        
        if let Some(limiter) = limiter_opt {
            let request_key = RpcRequestKey::new(route, &params);
            
            match limiter.check_rpc(&request_key) {
                Ok(()) => {
                    // 允许发送
                }
                Err(RpcRateLimitError::DuplicateRequest { method, pending_since }) => {
                    warn!(
                        "拦截重复 RPC 请求: {}, 已等待 {:?}",
                        method, pending_since
                    );
                    return Err(PrivchatSDKError::Other(format!(
                        "重复请求: {}, 请等待上一个请求完成",
                        method
                    )));
                }
                Err(RpcRateLimitError::RateLimitExceeded { method, wait_duration }) => {
                    info!(
                        "RPC 请求超限: {}, 自动等待 {}ms",
                        method,
                        wait_duration.as_millis()
                    );
                    tokio::time::sleep(wait_duration).await;
                }
            }
            
            // 调用底层 RPC 方法
            let result = self.call_rpc(route, params).await;
            
            // 🔥 标记请求完成（成功或失败都要调用）
            limiter.mark_complete(&request_key);
            
            // 处理结果
            let result = result?;
            
            // 反序列化响应
            let response: Res = serde_json::from_value(result)
                .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化响应失败: {}", e)))?;
            
            Ok(response)
        } else {
            // 没有限流器，直接调用
            let result = self.call_rpc(route, params).await?;
            
            // 反序列化响应
            let response: Res = serde_json::from_value(result)
                .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化响应失败: {}", e)))?;
            
            Ok(response)
        }
    }
}
