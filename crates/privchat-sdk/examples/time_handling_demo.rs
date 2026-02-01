//! 时区处理示例
//! 
//! 展示如何正确处理 UTC 时间戳和本地时区的转换

use privchat_sdk::utils::TimeFormatter;
use chrono::{Utc, Duration};

fn main() {
    println!("=== PrivChat SDK 时区处理示例 ===\n");
    
    // 1. 获取当前 UTC 时间戳（这是存储在数据库中的格式）
    let current_timestamp = TimeFormatter::now_utc_millis();
    println!("1️⃣ 当前 UTC 时间戳: {}", current_timestamp);
    println!("   （这是存储在数据库中的原始值）\n");
    
    // 2. 格式化消息时间（智能选择格式）
    println!("2️⃣ 智能消息时间格式化:");
    
    // 刚刚发送的消息
    let just_now = current_timestamp;
    println!("   刚刚: {}", TimeFormatter::format_message_time(just_now));
    
    // 5分钟前
    let five_min_ago = (Utc::now() - Duration::minutes(5)).timestamp_millis();
    println!("   5分钟前: {}", TimeFormatter::format_message_time(five_min_ago));
    
    // 2小时前
    let two_hours_ago = (Utc::now() - Duration::hours(2)).timestamp_millis();
    println!("   2小时前: {}", TimeFormatter::format_message_time(two_hours_ago));
    
    // 昨天
    let yesterday = (Utc::now() - Duration::days(1)).timestamp_millis();
    println!("   昨天: {}", TimeFormatter::format_message_time(yesterday));
    
    // 一周前
    let week_ago = (Utc::now() - Duration::days(7)).timestamp_millis();
    println!("   一周前: {}", TimeFormatter::format_message_time(week_ago));
    println!();
    
    // 3. 详细时间格式
    println!("3️⃣ 详细时间格式:");
    println!("   {}", TimeFormatter::format_detailed_time(current_timestamp));
    println!();
    
    // 4. 相对时间
    println!("4️⃣ 相对时间显示:");
    let timestamps = vec![
        (Utc::now() - Duration::minutes(1)).timestamp_millis(),
        (Utc::now() - Duration::hours(3)).timestamp_millis(),
        (Utc::now() - Duration::days(2)).timestamp_millis(),
        (Utc::now() - Duration::days(35)).timestamp_millis(),
    ];
    
    for ts in timestamps {
        println!("   {}", TimeFormatter::format_relative_time(ts));
    }
    println!();
    
    // 5. 日期和时间分离
    println!("5️⃣ 日期和时间分离:");
    println!("   日期: {}", TimeFormatter::format_date(current_timestamp));
    println!("   时间: {}", TimeFormatter::format_time(current_timestamp));
    println!();
    
    // 6. 本地时间字符串转 UTC 时间戳（用于用户输入）
    println!("6️⃣ 本地时间字符串转 UTC 时间戳:");
    let local_time_str = "2024-01-17 14:30:00";
    if let Some(utc_ts) = TimeFormatter::local_str_to_utc_timestamp(local_time_str) {
        println!("   输入: {}", local_time_str);
        println!("   转换为 UTC 时间戳: {}", utc_ts);
        println!("   验证（格式化回显）: {}", TimeFormatter::format_detailed_time(utc_ts));
    }
    println!();
    
    // 7. 判断是否在同一天
    println!("7️⃣ 判断消息是否在同一天（用于消息分组显示）:");
    let msg1_time = current_timestamp;
    let msg2_time = (Utc::now() - Duration::hours(2)).timestamp_millis();
    let msg3_time = (Utc::now() - Duration::days(1)).timestamp_millis();
    
    println!("   消息1 和 消息2（2小时前）同一天: {}", 
        TimeFormatter::is_same_day(msg1_time, msg2_time));
    println!("   消息1 和 消息3（昨天）同一天: {}", 
        TimeFormatter::is_same_day(msg1_time, msg3_time));
    println!();
    
    // 8. 模拟聊天消息列表显示
    println!("8️⃣ 模拟聊天消息列表显示:");
    println!("   ┌─────────────────────────────────────────┐");
    
    let messages = vec![
        ("Alice", "早上好！", (Utc::now() - Duration::hours(8)).timestamp_millis()),
        ("Bob", "中午一起吃饭吗？", (Utc::now() - Duration::hours(4)).timestamp_millis()),
        ("Alice", "好啊，去哪里？", (Utc::now() - Duration::hours(3)).timestamp_millis()),
        ("Bob", "楼下新开的餐厅", (Utc::now() - Duration::minutes(30)).timestamp_millis()),
        ("Alice", "好的，一会儿见", (Utc::now() - Duration::minutes(5)).timestamp_millis()),
    ];
    
    let mut last_date = String::new();
    for (sender, content, timestamp) in messages {
        let current_date = TimeFormatter::format_date(timestamp);
        
        // 如果日期变了，显示日期分隔符
        if current_date != last_date {
            println!("   │ ─────────── {} ─────────── │", current_date);
            last_date = current_date;
        }
        
        let time_str = TimeFormatter::format_message_time(timestamp);
        println!("   │ {:<10} [{}]                │", sender, time_str);
        println!("   │   {}                            │", content);
    }
    
    println!("   └─────────────────────────────────────────┘");
    println!();
    
    println!("✅ 时区处理原则:");
    println!("   • 存储层: 使用 UTC 毫秒时间戳（INTEGER）");
    println!("   • 业务层: 使用 Utc::now().timestamp_millis()");
    println!("   • UI 层: 使用 TimeFormatter 转换为本地时间显示");
    println!("   • 用户输入: 将本地时间转换为 UTC 后存储");
}
