//! 实体类型枚举 - ENTITY_SYNC_V1 受控枚举
//!
//! entity_type 为受控枚举，新增需 SDK 与 Server 同步升级。

use std::str::FromStr;

/// 实体类型（与 ENTITY_SYNC_V1 协议一致）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityType {
    Friend,
    Group,
    Channel,
    GroupMember,
    User,
    UserSettings,
    UserBlock,
}

impl EntityType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Friend => "friend",
            Self::Group => "group",
            Self::Channel => "channel",
            Self::GroupMember => "group_member",
            Self::User => "user",
            Self::UserSettings => "user_settings",
            Self::UserBlock => "user_block",
        }
    }

    /// 单实体刷新时是否不推进集合级 cursor（见 ENTITY_SYNC_V1 硬约束）
    pub fn is_single_entity_refresh_when_scoped(self, scope: Option<&str>) -> bool {
        if scope.is_none() {
            return false;
        }
        matches!(self, Self::User | Self::Group | Self::Channel)
    }
}

impl FromStr for EntityType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "friend" => Ok(Self::Friend),
            "group" => Ok(Self::Group),
            "channel" => Ok(Self::Channel),
            "group_member" => Ok(Self::GroupMember),
            "user" => Ok(Self::User),
            "user_settings" => Ok(Self::UserSettings),
            "user_block" => Ok(Self::UserBlock),
            _ => Err(()),
        }
    }
}

impl std::fmt::Display for EntityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn entity_type_as_str_and_from_str() {
        assert_eq!(EntityType::Friend.as_str(), "friend");
        assert_eq!(EntityType::Group.as_str(), "group");
        assert_eq!(EntityType::User.as_str(), "user");
        assert_eq!(EntityType::from_str("friend").unwrap(), EntityType::Friend);
        assert_eq!(EntityType::from_str("group_member").unwrap(), EntityType::GroupMember);
        assert!(EntityType::from_str("unknown").is_err());
    }

    #[test]
    fn single_entity_refresh_does_not_advance_cursor() {
        // user / group / channel 带 scope 时 = 单实体刷新，不写集合级 cursor
        assert!(EntityType::User.is_single_entity_refresh_when_scoped(Some("123")));
        assert!(EntityType::Group.is_single_entity_refresh_when_scoped(Some("456")));
        assert!(EntityType::Channel.is_single_entity_refresh_when_scoped(Some("789")));
        assert!(!EntityType::User.is_single_entity_refresh_when_scoped(None));
        // group_member 带 scope 是「按群同步成员」，要写 cursor
        assert!(!EntityType::GroupMember.is_single_entity_refresh_when_scoped(Some("g1")));
    }
}
