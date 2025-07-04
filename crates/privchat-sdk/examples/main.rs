use privchat_sdk::{PrivchatClient, SendRequest};
use std::sync::Arc;

/// PrivchatClient 使用示例
/// 
/// 演示新的架构设计：
/// 1. PrivchatClient::new(work_dir, transport) - 传入工作目录
/// 2. client.connect(login, token) - 连接后获取服务器返回的 UID
/// 3. SDK 自动创建用户目录和数据库
/// 4. 后续所有操作都基于用户目录
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 PrivchatClient 新架构示例");
    
    // 展示正确的使用流程
    println!("\n📋 正确的使用流程:");
    println!("   1️⃣ 创建 PrivchatClient - 只传入工作根目录");
    println!("   2️⃣ 调用 connect() - 服务器返回真实的 UID");
    println!("   3️⃣ SDK 自动创建用户目录: ~/.privchat/u_<UID>/");
    println!("   4️⃣ 初始化数据库和媒体目录");
    println!("   5️⃣ 所有后续操作都基于用户目录");
    
    // 目录结构说明
    println!("\n📁 目录结构示例:");
    println!("   ~/.privchat/");
    println!("   ├── u_2489dks83/          # 服务器返回的用户 UID");
    println!("   │   ├── messages.db       # 加密聊天记录数据库 (SQLCipher)");
    println!("   │   ├── messages/         # 消息缓存");
    println!("   │   ├── media/            # 媒体文件");
    println!("   │   │   ├── images/       # 图片");
    println!("   │   │   ├── videos/       # 视频");
    println!("   │   │   └── audios/       # 音频");
    println!("   │   ├── files/            # 附件文件");
    println!("   │   └── cache/            # 临时缓存");
    println!("   └── u_8392kd92/           # 另一个用户账号");
    
    // 代码示例（注释形式，因为需要真实的Transport）
    println!("\n💻 代码示例:");
    println!(r#"
    // 1. 创建客户端 - 只传入工作根目录，不需要知道用户ID
    let mut client = PrivchatClient::new(
        "/home/user/.privchat",
        transport
    ).await?;
    
    // 2. 连接并登录 - 服务器返回真实的UID
    client.connect("13800001111", "sms_token_abc").await?;
    // 此时SDK已创建: ~/.privchat/u_123456/ 目录和数据库
    
    // 3. 获取用户信息
    let user_id = client.user_id().unwrap();
    let user_dir = client.user_dir().unwrap();
         println!("用户ID: {{user_id}}");
     println!("用户目录: {{user_dir}}");
    
    // 4. 发送消息 - 自动保存到数据库
    let mut message = SendRequest::new();
    message.from_uid = user_id.to_string();
    message.channel_id = "channel_123".to_string();
    message.payload = "Hello, World!".as_bytes().to_vec();
    
    client.send_chat_message(&message).await?;
    
    // 5. 其他操作
    client.subscribe_channel("news_channel").await?;
    client.send_ping().await?;
    
    // 6. 断开连接 - 自动清理资源
    client.disconnect("用户主动退出").await?;
    "#);
    
    // 架构优势说明
    println!("\n✅ 新架构的优势:");
    println!("   🎯 时机正确: 在不知道UID时不创建用户目录");
    println!("   🔐 安全性: UID来自服务器验证，不可伪造");
    println!("   🔒 数据加密: 使用 SQLCipher 加密数据库，保护隐私");
    println!("   📦 自动化: 目录、数据库、缓存自动管理");
    println!("   🔄 状态管理: 连接状态与用户数据绑定");
    println!("   👥 多用户: 支持同根目录下多用户账号");
    println!("   💾 持久化: 聊天记录、设置、媒体文件持久存储");
    
    // 注意事项
    println!("\n⚠️  注意事项:");
    println!("   • 需要真实的 msgtrans Transport 实例才能运行");
    println!("   • connect() 方法会修改客户端状态（需要 &mut self）");
    println!("   • 数据库连接会在 disconnect() 时自动关闭");
    println!("   • 媒体文件需要应用层主动管理路径");
    println!("   • 加密数据库使用用户ID派生密钥，确保安全性");
    println!("   • 无法使用 sqlite3 CLI 直接查看加密数据库");
    
    println!("\n🎉 PrivchatClient 新架构展示完成！");
    
    Ok(())
}
