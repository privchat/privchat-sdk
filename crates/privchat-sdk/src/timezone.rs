// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! 显示时区配置与 UTC→本地格式化（TIMEZONE_SPEC）。
//!
//! 硬约束（local-first + spec §1/§4）：
//! - 存储/传输/数据模型的时间戳永远是 **UTC 毫秒**，本模块绝不偏移后回写数据层。
//! - 时区转换只发生在格式化函数内部：输入 UTC 毫秒 + IANA ID，输出字符串/本地结构体。
//! - 配置是本地显示偏好（`DisplayTimezone`），不触网；格式化是纯本地计算。
//! - 禁止用固定 offset 代替 IANA ID —— 转换走 `chrono-tz` 时区数据库（含 DST/历史规则）。
//!
//! "今天/昨天/本周" 的判定基于**配置时区下的日历日**，不是 UTC 日。
//! "本周" 采用与 privchat-ui Formatter 一致的滚动 7 天窗口（`ts >= now - 7d`）。

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// 时区/时间戳错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimezoneError {
    /// 不是合法的 IANA 时区 ID。
    #[error("unknown IANA timezone id: {0}")]
    UnknownZone(String),
    /// UTC 毫秒超出可表示范围。
    #[error("timestamp out of range: {0}")]
    InvalidTimestamp(i64),
}

/// 本地日期时间结构体（TIMEZONE_SPEC §3.3 原子能力输出）。
///
/// `day_of_week` 采用 ISO 约定：周一=1 … 周日=7。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTimeParts {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub day_of_week: u32,
}

fn parse_zone(zone_id: &str) -> Result<Tz, TimezoneError> {
    zone_id
        .parse::<Tz>()
        .map_err(|_| TimezoneError::UnknownZone(zone_id.to_string()))
}

fn to_local(millis: i64, zone: Tz) -> Result<DateTime<Tz>, TimezoneError> {
    Utc.timestamp_millis_opt(millis)
        .single()
        .map(|dt| dt.with_timezone(&zone))
        .ok_or(TimezoneError::InvalidTimestamp(millis))
}

/// UTC 毫秒 → 指定 IANA 时区的本地日期时间结构体（原子能力，供上层自行格式化）。
///
/// 上层如需本地化文案，应基于本结构体 + 自己的 i18n 组装，不要自行加减偏移（spec §4）。
pub fn epoch_millis_to_local_datetime(
    millis: i64,
    zone_id: &str,
) -> Result<LocalDateTimeParts, TimezoneError> {
    let zone = parse_zone(zone_id)?;
    let dt = to_local(millis, zone)?;
    Ok(LocalDateTimeParts {
        year: dt.year(),
        month: dt.month(),
        day: dt.day(),
        hour: dt.hour(),
        minute: dt.minute(),
        second: dt.second(),
        day_of_week: dt.weekday().number_from_monday(),
    })
}

/// 设备系统时区的 IANA ID（原子能力）。取不到时回退 `"UTC"`。
pub fn system_default_timezone_id() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// 显示时区配置（本地显示偏好，仅影响格式化输出，不影响任何数据模型时间戳）。
///
/// 默认设备系统时区；`set` 校验 IANA ID，非法直接拒绝、不改变当前配置。
#[derive(Debug, Clone)]
pub struct DisplayTimezone {
    zone_id: String,
}

impl DisplayTimezone {
    /// 以设备系统时区初始化。
    pub fn new_system_default() -> Self {
        Self {
            zone_id: system_default_timezone_id(),
        }
    }

    /// 以指定 IANA ID 初始化（校验合法性）。
    pub fn new(zone_id: &str) -> Result<Self, TimezoneError> {
        parse_zone(zone_id)?;
        Ok(Self {
            zone_id: zone_id.to_string(),
        })
    }

    /// 设置显示时区（IANA ID）。非法 ID 返回错误且保留原配置。
    pub fn set(&mut self, zone_id: &str) -> Result<(), TimezoneError> {
        parse_zone(zone_id)?;
        self.zone_id = zone_id.to_string();
        Ok(())
    }

    /// 当前显示时区 ID。
    pub fn get(&self) -> &str {
        &self.zone_id
    }

    /// 重置为设备系统默认时区。
    pub fn reset(&mut self) {
        self.zone_id = system_default_timezone_id();
    }
}

const WEEKDAY_ZH: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

fn hh_mm(dt: &DateTime<Tz>) -> String {
    format!("{:02}:{:02}", dt.hour(), dt.minute())
}

/// 消息气泡时间 `"HH:mm"`（spec §3.2）。`millis <= 0` 返回空串。
pub fn format_message_time(millis: i64, zone_id: &str) -> Result<String, TimezoneError> {
    if millis <= 0 {
        return Ok(String::new());
    }
    let zone = parse_zone(zone_id)?;
    let dt = to_local(millis, zone)?;
    Ok(hh_mm(&dt))
}

/// 会话列表时间（spec §3.2）：今天 `"HH:mm"` / 昨天 `"昨天"` / 本周（7 天内）`"周X"` / 更早 `"MM-dd"`。
///
/// `now_millis` 显式传入以便确定性测试；SDK 方法用 `Utc::now()`。
/// "今天/昨天" 按配置时区日历日判定，"本周" 用滚动 7 天窗口（与 privchat-ui 一致）。
pub fn format_conversation_time(
    millis: i64,
    zone_id: &str,
    now_millis: i64,
) -> Result<String, TimezoneError> {
    if millis <= 0 {
        return Ok(String::new());
    }
    let zone = parse_zone(zone_id)?;
    let ts = to_local(millis, zone)?;
    let now = to_local(now_millis, zone)?;
    let ts_date = ts.date_naive();
    if ts_date == now.date_naive() {
        return Ok(hh_mm(&ts));
    }
    let yesterday = to_local(now_millis - 86_400_000, zone)?.date_naive();
    if ts_date == yesterday {
        return Ok("昨天".to_string());
    }
    if millis >= now_millis - 7 * 86_400_000 {
        let idx = ts.weekday().number_from_monday() as usize - 1;
        return Ok(WEEKDAY_ZH[idx].to_string());
    }
    Ok(format!("{:02}-{:02}", ts.month(), ts.day()))
}

/// 消息时间分隔符（spec §3.2）：今天 `"今天 HH:mm"` / 昨天 `"昨天 HH:mm"` /
/// 同年 `"MM月dd日 HH:mm"` / 跨年 `"yyyy年MM月dd日 HH:mm"`。
pub fn format_separator_time(
    millis: i64,
    zone_id: &str,
    now_millis: i64,
) -> Result<String, TimezoneError> {
    if millis <= 0 {
        return Ok(String::new());
    }
    let zone = parse_zone(zone_id)?;
    let ts = to_local(millis, zone)?;
    let now = to_local(now_millis, zone)?;
    let time = hh_mm(&ts);
    let ts_date = ts.date_naive();
    if ts_date == now.date_naive() {
        return Ok(format!("今天 {time}"));
    }
    let yesterday = to_local(now_millis - 86_400_000, zone)?.date_naive();
    if ts_date == yesterday {
        return Ok(format!("昨天 {time}"));
    }
    if ts.year() == now.year() {
        return Ok(format!("{:02}月{:02}日 {time}", ts.month(), ts.day()));
    }
    Ok(format!(
        "{}年{:02}月{:02}日 {time}",
        ts.year(),
        ts.month(),
        ts.day()
    ))
}

// SDK 实例方法：薄封装，读取实例配置时区后委托上面的纯函数。
// 放在本模块（crate root 的子模块）即可访问 PrivchatSdk 的私有字段。
impl crate::PrivchatSdk {
    /// 设置显示时区（IANA ID）。非法 ID 返回错误且保留原配置。
    pub fn set_display_timezone(&self, zone_id: &str) -> Result<(), TimezoneError> {
        self.display_timezone
            .lock()
            .expect("display_timezone mutex poisoned")
            .set(zone_id)
    }

    /// 获取当前显示时区 ID。
    pub fn get_display_timezone(&self) -> String {
        self.display_timezone
            .lock()
            .expect("display_timezone mutex poisoned")
            .get()
            .to_string()
    }

    /// 重置显示时区为设备系统默认。
    pub fn reset_display_timezone(&self) {
        self.display_timezone
            .lock()
            .expect("display_timezone mutex poisoned")
            .reset();
    }

    /// 消息气泡时间 `"HH:mm"`，按实例配置时区。
    pub fn format_message_time(&self, millis: i64) -> Result<String, TimezoneError> {
        let zone = self.get_display_timezone();
        format_message_time(millis, &zone)
    }

    /// 会话列表时间，按实例配置时区，"现在" 取 `Utc::now()`。
    pub fn format_conversation_time(&self, millis: i64) -> Result<String, TimezoneError> {
        let zone = self.get_display_timezone();
        format_conversation_time(millis, &zone, Utc::now().timestamp_millis())
    }

    /// 消息时间分隔符，按实例配置时区，"现在" 取 `Utc::now()`。
    pub fn format_separator_time(&self, millis: i64) -> Result<String, TimezoneError> {
        let zone = self.get_display_timezone();
        format_separator_time(millis, &zone, Utc::now().timestamp_millis())
    }

    /// UTC 毫秒 → 指定时区本地日期时间结构体（原子能力，供上层本地化格式化）。
    pub fn epoch_millis_to_local_datetime(
        &self,
        millis: i64,
        zone_id: &str,
    ) -> Result<LocalDateTimeParts, TimezoneError> {
        epoch_millis_to_local_datetime(millis, zone_id)
    }

    /// 设备系统时区 IANA ID（原子能力）。
    pub fn system_default_timezone_id(&self) -> String {
        system_default_timezone_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 由 UTC 日历分量构造毫秒时间戳（测试夹具，确定性）。
    fn utc_millis(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s)
            .single()
            .expect("valid utc fixture")
            .timestamp_millis()
    }

    #[test]
    fn atom_epoch_zero_utc() {
        let p = epoch_millis_to_local_datetime(0, "UTC").unwrap();
        assert_eq!(
            p,
            LocalDateTimeParts {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
                day_of_week: 4, // 1970-01-01 是周四
            }
        );
    }

    #[test]
    fn atom_shanghai_is_utc_plus_8() {
        let p = epoch_millis_to_local_datetime(0, "Asia/Shanghai").unwrap();
        assert_eq!((p.year, p.month, p.day, p.hour), (1970, 1, 1, 8));
        assert_eq!(p.day_of_week, 4);
    }

    #[test]
    fn atom_new_york_dst_summer_vs_winter() {
        // 2026-07-01 12:00 UTC → EDT(-4) → 08:00；2026-01-01 12:00 UTC → EST(-5) → 07:00。
        let summer = utc_millis(2026, 7, 1, 12, 0, 0);
        let winter = utc_millis(2026, 1, 1, 12, 0, 0);
        let s = epoch_millis_to_local_datetime(summer, "America/New_York").unwrap();
        let w = epoch_millis_to_local_datetime(winter, "America/New_York").unwrap();
        assert_eq!(s.hour, 8, "DST 夏令时应为 UTC-4");
        assert_eq!(w.hour, 7, "冬令时应为 UTC-5");
        // 固定 +8 会在纽约冬夏都错；这里证明走的是 IANA 规则而非固定 offset。
        assert_ne!(s.hour, w.hour);
    }

    #[test]
    fn atom_ho_chi_minh_no_dst() {
        // 胡志明市 UTC+7，全年无 DST。
        let summer = utc_millis(2026, 7, 1, 12, 0, 0);
        let winter = utc_millis(2026, 1, 1, 12, 0, 0);
        assert_eq!(
            epoch_millis_to_local_datetime(summer, "Asia/Ho_Chi_Minh").unwrap().hour,
            19
        );
        assert_eq!(
            epoch_millis_to_local_datetime(winter, "Asia/Ho_Chi_Minh").unwrap().hour,
            19
        );
    }

    #[test]
    fn atom_rejects_unknown_zone() {
        assert_eq!(
            epoch_millis_to_local_datetime(0, "Mars/Olympus"),
            Err(TimezoneError::UnknownZone("Mars/Olympus".into()))
        );
        // 固定 offset 不是 IANA ID，必须拒绝（spec §1 禁止固定 offset）。
        assert!(epoch_millis_to_local_datetime(0, "+8").is_err());
        assert!(epoch_millis_to_local_datetime(0, "+08:00").is_err());
    }

    #[test]
    fn system_default_is_valid_iana() {
        let id = system_default_timezone_id();
        assert!(!id.is_empty());
        assert!(id.parse::<Tz>().is_ok(), "系统时区应为合法 IANA ID: {id}");
    }

    #[test]
    fn display_timezone_set_get_reset() {
        let mut tz = DisplayTimezone::new("UTC").unwrap();
        assert_eq!(tz.get(), "UTC");
        tz.set("Asia/Ho_Chi_Minh").unwrap();
        assert_eq!(tz.get(), "Asia/Ho_Chi_Minh");
        // 非法 ID 拒绝且保留原配置。
        assert!(tz.set("Not/AZone").is_err());
        assert_eq!(tz.get(), "Asia/Ho_Chi_Minh");
        tz.reset();
        assert_eq!(tz.get(), system_default_timezone_id());
    }

    #[test]
    fn message_time_hh_mm() {
        let ts = utc_millis(2026, 11, 12, 14, 5, 0); // 14:05 UTC
        assert_eq!(format_message_time(ts, "UTC").unwrap(), "14:05");
        assert_eq!(format_message_time(ts, "Asia/Shanghai").unwrap(), "22:05");
        assert_eq!(format_message_time(ts, "America/New_York").unwrap(), "09:05"); // EST -5
    }

    #[test]
    fn non_positive_returns_empty() {
        assert_eq!(format_message_time(0, "UTC").unwrap(), "");
        assert_eq!(format_message_time(-1, "UTC").unwrap(), "");
        assert_eq!(format_conversation_time(0, "UTC", 1).unwrap(), "");
        assert_eq!(format_separator_time(-5, "UTC", 1).unwrap(), "");
    }

    // 会话时间：固定 now = 2026-11-12 12:00 UTC（周四），时区 UTC。
    fn now_utc() -> i64 {
        utc_millis(2026, 11, 12, 12, 0, 0)
    }

    #[test]
    fn conversation_time_today_yesterday_week_older() {
        let now = now_utc(); // 2026-11-12 周四
        // 今天 09:30 → HH:mm
        let today = utc_millis(2026, 11, 12, 9, 30, 0);
        assert_eq!(format_conversation_time(today, "UTC", now).unwrap(), "09:30");
        // 昨天 2026-11-11 → 昨天
        let yest = utc_millis(2026, 11, 11, 23, 0, 0);
        assert_eq!(format_conversation_time(yest, "UTC", now).unwrap(), "昨天");
        // 本周内（7 天）2026-11-09 周一 → 周一
        let mon = utc_millis(2026, 11, 9, 10, 0, 0);
        assert_eq!(format_conversation_time(mon, "UTC", now).unwrap(), "周一");
        // 更早（>7 天）2026-10-01 → MM-dd
        let old = utc_millis(2026, 10, 1, 10, 0, 0);
        assert_eq!(format_conversation_time(old, "UTC", now).unwrap(), "10-01");
    }

    #[test]
    fn conversation_time_week_window_is_rolling_7_days() {
        let now = now_utc(); // 2026-11-12 12:00
        // 恰好 7 天内（2026-11-06 12:00，周五）→ 周五
        let edge = utc_millis(2026, 11, 6, 12, 0, 0);
        assert_eq!(format_conversation_time(edge, "UTC", now).unwrap(), "周五");
        // 刚过 7 天（2026-11-05 11:59）→ MM-dd
        let past = utc_millis(2026, 11, 5, 11, 59, 0);
        assert_eq!(format_conversation_time(past, "UTC", now).unwrap(), "11-05");
    }

    #[test]
    fn separator_time_today_yesterday_sameyear_crossyear() {
        let now = now_utc(); // 2026-11-12
        let today = utc_millis(2026, 11, 12, 9, 30, 0);
        assert_eq!(format_separator_time(today, "UTC", now).unwrap(), "今天 09:30");
        let yest = utc_millis(2026, 11, 11, 23, 0, 0);
        assert_eq!(format_separator_time(yest, "UTC", now).unwrap(), "昨天 23:00");
        let same = utc_millis(2026, 3, 5, 8, 15, 0);
        assert_eq!(format_separator_time(same, "UTC", now).unwrap(), "03月05日 08:15");
        let cross = utc_millis(2025, 12, 31, 23, 59, 0);
        assert_eq!(
            format_separator_time(cross, "UTC", now).unwrap(),
            "2025年12月31日 23:59"
        );
    }

    #[test]
    fn calendar_day_boundary_uses_configured_zone_not_utc() {
        // 纽约午夜 = 05:00 UTC，上海午夜 = 16:00 UTC。取 now/ts 跨越纽约午夜但不跨上海午夜：
        //   now = 2026-11-12 05:30 UTC，ts = 2026-11-12 04:30 UTC（相差 1 小时）。
        //   Shanghai(+8)：now=11-12 13:30、ts=11-12 12:30 → 同一日历日 → 今天 HH:mm。
        //   New_York(-5 EST)：now=11-12 00:30、ts=11-11 23:30 → ts 落在昨天 → 昨天。
        // 同一对 UTC 毫秒在不同配置时区得到不同的相对日分类，证明判定基于配置时区而非 UTC。
        let now = utc_millis(2026, 11, 12, 5, 30, 0);
        let ts = utc_millis(2026, 11, 12, 4, 30, 0);
        assert_eq!(format_conversation_time(ts, "Asia/Shanghai", now).unwrap(), "12:30");
        assert_eq!(format_conversation_time(ts, "America/New_York", now).unwrap(), "昨天");
        assert_eq!(
            format_separator_time(ts, "Asia/Shanghai", now).unwrap(),
            "今天 12:30"
        );
        assert_eq!(
            format_separator_time(ts, "America/New_York", now).unwrap(),
            "昨天 23:30"
        );
    }

    #[test]
    fn data_model_timestamps_stay_utc() {
        // 配置时区只影响格式化输出，原子能力对同一 UTC 毫秒在不同时区给出不同本地分量，
        // 但输入的 UTC 毫秒本身不被改写（这里验证转换是纯函数、可逆推回 UTC）。
        let millis = utc_millis(2026, 11, 12, 14, 5, 0);
        let sh = epoch_millis_to_local_datetime(millis, "Asia/Shanghai").unwrap();
        let reconstructed = Utc
            .with_ymd_and_hms(sh.year, sh.month, sh.day, sh.hour, sh.minute, sh.second)
            .single()
            .unwrap()
            .timestamp_millis();
        // 上海分量比 UTC 多 8 小时，回推应正好差 8h，证明没有污染原始 UTC 值。
        assert_eq!(reconstructed - millis, 8 * 3_600_000);
    }

    #[test]
    fn invalid_zone_propagates_in_formatters() {
        let ts = utc_millis(2026, 11, 12, 14, 5, 0);
        assert!(format_message_time(ts, "Bad/Zone").is_err());
        assert!(format_conversation_time(ts, "Bad/Zone", ts).is_err());
        assert!(format_separator_time(ts, "Bad/Zone", ts).is_err());
    }
}