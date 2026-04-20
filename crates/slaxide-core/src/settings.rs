use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{models::TimelineItem, theme::ThemeId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAction {
    Notify,
    Silent,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietHours {
    pub start_hour: u8,
    pub end_hour: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternMatcher {
    Text(String),
    Regex(String),
}

impl PatternMatcher {
    pub fn matches(&self, haystack: &str) -> Result<bool, regex::Error> {
        match self {
            Self::Text(text) => Ok(haystack.to_lowercase().contains(&text.to_lowercase())),
            Self::Regex(pattern) => Ok(Regex::new(pattern)?.is_match(haystack)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileMode {
    #[default]
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPermission {
    #[default]
    ReadWrite,
    ReadOnly,
    Hidden,
}

impl ChannelPermission {
    pub fn can_read(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    pub fn can_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeywordProfile {
    pub id: String,
    pub label: String,
    pub mode: ProfileMode,
    pub matchers: Vec<PatternMatcher>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChannelProfile {
    pub id: String,
    pub label: String,
    pub mode: ProfileMode,
    pub channels: BTreeSet<String>,
    pub channel_name_matchers: Vec<PatternMatcher>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SectionProfile {
    pub id: String,
    pub label: String,
    pub channels: BTreeSet<String>,
    pub channel_name_matchers: Vec<PatternMatcher>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthorProfile {
    pub id: String,
    pub label: String,
    pub mode: ProfileMode,
    pub authors: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchProfile {
    pub id: String,
    pub label: String,
    pub query: Option<String>,
    pub keyword_profiles: Vec<String>,
    pub section_profiles: Vec<String>,
    pub channel_profiles: Vec<String>,
    pub author_profiles: Vec<String>,
    pub channels: BTreeSet<String>,
    pub authors: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRule {
    pub id: Uuid,
    pub label: String,
    pub enabled: bool,
    pub channels: BTreeSet<String>,
    pub authors: BTreeSet<String>,
    pub include: Vec<PatternMatcher>,
    pub exclude: Vec<PatternMatcher>,
    #[serde(default)]
    pub keyword_profile_ids: Vec<String>,
    #[serde(default)]
    pub section_profile_ids: Vec<String>,
    #[serde(default)]
    pub channel_profile_ids: Vec<String>,
    #[serde(default)]
    pub author_profile_ids: Vec<String>,
    #[serde(default)]
    pub search_profile_ids: Vec<String>,
    pub thread_participation_only: bool,
    pub quiet_hours: Option<QuietHours>,
    pub action: NotificationAction,
}

impl NotificationRule {
    pub fn validate(&self) -> Result<(), SettingsError> {
        for matcher in self.include.iter().chain(self.exclude.iter()) {
            if let PatternMatcher::Regex(pattern) = matcher {
                Regex::new(pattern).map_err(|source| SettingsError::InvalidRegex {
                    pattern: pattern.clone(),
                    source,
                })?;
            }
        }

        if let Some(quiet_hours) = &self.quiet_hours
            && (quiet_hours.start_hour > 23 || quiet_hours.end_hour > 23)
        {
            return Err(SettingsError::InvalidQuietHours);
        }

        Ok(())
    }

    pub fn matches(&self, item: &TimelineItem) -> Result<bool, SettingsError> {
        self.validate()?;

        if !self.enabled {
            return Ok(false);
        }

        if !self.channels.is_empty() && !self.channels.contains(&item.channel_id) {
            return Ok(false);
        }

        if !self.authors.is_empty() && !self.authors.contains(&item.author_id) {
            return Ok(false);
        }

        if self.thread_participation_only && !item.participant {
            return Ok(false);
        }

        if !self.include.is_empty() {
            let include_match = self
                .include
                .iter()
                .try_fold(false, |matched, matcher| {
                    if matched {
                        Ok(true)
                    } else {
                        matcher.matches(&item.body)
                    }
                })
                .map_err(|source| SettingsError::Matcher(source.to_string()))?;

            if !include_match {
                return Ok(false);
            }
        }

        let exclude_match = self
            .exclude
            .iter()
            .try_fold(false, |matched, matcher| {
                if matched {
                    Ok(true)
                } else {
                    matcher.matches(&item.body)
                }
            })
            .map_err(|source| SettingsError::Matcher(source.to_string()))?;

        Ok(!exclude_match)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProfile {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlackConfigSettings {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
    pub user_scopes: Option<String>,
    pub app_token: Option<String>,
}

impl SlackConfigSettings {
    pub fn apply_patch(&mut self, patch: Self) {
        if let Some(client_id) = patch.client_id {
            self.client_id = normalize_optional_id(client_id);
        }
        if let Some(client_secret) = patch.client_secret {
            self.client_secret = normalize_optional_id(client_secret);
        }
        if let Some(redirect_uri) = patch.redirect_uri {
            self.redirect_uri = normalize_optional_id(redirect_uri);
        }
        if let Some(user_scopes) = patch.user_scopes {
            self.user_scopes = normalize_optional_id(user_scopes);
        }
        if let Some(app_token) = patch.app_token {
            self.app_token = normalize_optional_id(app_token);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OfficeSettings {
    pub channel_profile_id: Option<String>,
}

impl OfficeSettings {
    pub fn apply_patch(&mut self, patch: Self) {
        if let Some(channel_profile_id) = patch.channel_profile_id {
            self.channel_profile_id = normalize_optional_id(channel_profile_id);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShortcutBindings {
    pub open_settings: Vec<String>,
    pub open_admin: Vec<String>,
    pub focus_search: Vec<String>,
    pub focus_composer: Vec<String>,
    pub close_column: Vec<String>,
}

impl Default for ShortcutBindings {
    fn default() -> Self {
        Self {
            open_settings: vec!["<Ctrl>comma".to_string()],
            open_admin: vec!["<Ctrl><Shift>a".to_string()],
            focus_search: vec!["<Ctrl>k".to_string()],
            focus_composer: vec!["<Ctrl>n".to_string()],
            close_column: vec!["<Ctrl>w".to_string(), "Escape".to_string()],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimelinePolicy {
    pub watched_channels: BTreeSet<String>,
    pub muted_channels: BTreeSet<String>,
    pub channel_weights: BTreeMap<String, u8>,
    pub focus_keywords: Vec<String>,
    pub focus_threshold: f64,
    pub recent_window_days: u16,
}

impl Default for TimelinePolicy {
    fn default() -> Self {
        Self {
            watched_channels: BTreeSet::new(),
            muted_channels: BTreeSet::new(),
            channel_weights: BTreeMap::new(),
            focus_keywords: Vec::new(),
            focus_threshold: 75.0,
            recent_window_days: 7,
        }
    }
}

impl TimelinePolicy {
    pub fn is_watched(&self, channel_id: &str) -> bool {
        self.watched_channels.contains(channel_id)
    }

    pub fn is_effectively_watched(&self, channel_id: &str) -> bool {
        self.watched_channels.is_empty() || self.is_watched(channel_id)
    }

    pub fn weight_for(&self, channel_id: &str) -> u8 {
        self.channel_weights.get(channel_id).copied().unwrap_or(1)
    }

    pub fn matching_keywords(&self, body: &str) -> Vec<String> {
        let body = body.to_lowercase();
        self.focus_keywords
            .iter()
            .filter(|keyword| body.contains(&keyword.to_lowercase()))
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettingsPatch {
    pub theme_id: Option<ThemeId>,
    pub timeline: Option<TimelinePolicy>,
    pub notification_rules: Option<Vec<NotificationRule>>,
    pub slack: Option<SlackConfigSettings>,
    pub office: Option<OfficeSettings>,
    pub shortcuts: Option<ShortcutBindings>,
    pub active_workspace_key: Option<String>,
    pub workspaces: Option<Vec<WorkspaceProfile>>,
    pub active_search_profile_id: Option<String>,
    pub keyword_profiles: Option<Vec<KeywordProfile>>,
    pub section_profiles: Option<Vec<SectionProfile>>,
    pub channel_profiles: Option<Vec<ChannelProfile>>,
    pub author_profiles: Option<Vec<AuthorProfile>>,
    pub search_profiles: Option<Vec<SearchProfile>>,
    pub default_channel_permission: Option<ChannelPermission>,
    pub channel_permissions: Option<BTreeMap<String, ChannelPermission>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme_id: ThemeId,
    pub timeline: TimelinePolicy,
    pub notification_rules: Vec<NotificationRule>,
    #[serde(default)]
    pub slack: SlackConfigSettings,
    #[serde(default)]
    pub office: OfficeSettings,
    #[serde(default)]
    pub shortcuts: ShortcutBindings,
    #[serde(default)]
    pub active_workspace_key: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceProfile>,
    #[serde(default)]
    pub active_search_profile_id: Option<String>,
    #[serde(default)]
    pub keyword_profiles: Vec<KeywordProfile>,
    #[serde(default)]
    pub section_profiles: Vec<SectionProfile>,
    #[serde(default)]
    pub channel_profiles: Vec<ChannelProfile>,
    #[serde(default)]
    pub author_profiles: Vec<AuthorProfile>,
    #[serde(default)]
    pub search_profiles: Vec<SearchProfile>,
    #[serde(default)]
    pub default_channel_permission: ChannelPermission,
    #[serde(default)]
    pub channel_permissions: BTreeMap<String, ChannelPermission>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme_id: ThemeId::TokyoNightStorm,
            timeline: TimelinePolicy::default(),
            notification_rules: Vec::new(),
            slack: SlackConfigSettings::default(),
            office: OfficeSettings::default(),
            shortcuts: ShortcutBindings::default(),
            active_workspace_key: None,
            workspaces: Vec::new(),
            active_search_profile_id: None,
            keyword_profiles: Vec::new(),
            section_profiles: Vec::new(),
            channel_profiles: Vec::new(),
            author_profiles: Vec::new(),
            search_profiles: Vec::new(),
            default_channel_permission: ChannelPermission::ReadWrite,
            channel_permissions: BTreeMap::new(),
        }
    }
}

impl AppSettings {
    pub fn apply_patch(&mut self, patch: AppSettingsPatch) {
        if let Some(theme_id) = patch.theme_id {
            self.theme_id = theme_id;
        }
        if let Some(timeline) = patch.timeline {
            self.timeline = timeline;
        }
        if let Some(notification_rules) = patch.notification_rules {
            self.notification_rules = notification_rules;
        }
        if let Some(slack) = patch.slack {
            self.slack.apply_patch(slack);
        }
        if let Some(office) = patch.office {
            self.office.apply_patch(office);
        }
        if let Some(shortcuts) = patch.shortcuts {
            self.shortcuts = shortcuts;
        }
        if let Some(active_workspace_key) = patch.active_workspace_key {
            self.active_workspace_key = normalize_optional_id(active_workspace_key);
        }
        if let Some(workspaces) = patch.workspaces {
            self.workspaces = workspaces;
        }
        if let Some(active_search_profile_id) = patch.active_search_profile_id {
            self.active_search_profile_id = normalize_optional_id(active_search_profile_id);
        }
        if let Some(keyword_profiles) = patch.keyword_profiles {
            self.keyword_profiles = keyword_profiles;
        }
        if let Some(section_profiles) = patch.section_profiles {
            self.section_profiles = section_profiles;
        }
        if let Some(channel_profiles) = patch.channel_profiles {
            self.channel_profiles = channel_profiles;
        }
        if let Some(author_profiles) = patch.author_profiles {
            self.author_profiles = author_profiles;
        }
        if let Some(search_profiles) = patch.search_profiles {
            self.search_profiles = search_profiles;
        }
        if let Some(default_channel_permission) = patch.default_channel_permission {
            self.default_channel_permission = default_channel_permission;
        }
        if let Some(channel_permissions) = patch.channel_permissions {
            self.channel_permissions = channel_permissions;
        }
    }

    pub fn search_profile(&self, profile_id: &str) -> Option<&SearchProfile> {
        self.search_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
    }

    pub fn section_profile(&self, profile_id: &str) -> Option<&SectionProfile> {
        self.section_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
    }

    pub fn channel_matches_active_search_profile(
        &self,
        channel_id: &str,
        channel_name: &str,
    ) -> Result<bool, SettingsError> {
        let Some(profile_id) = self.active_search_profile_id.as_deref() else {
            return Ok(false);
        };
        let profile = self.search_profile(profile_id).ok_or_else(|| {
            SettingsError::MissingProfileReference {
                kind: "search_profile".to_string(),
                id: profile_id.to_string(),
            }
        })?;
        profile_channel_matches_candidate(self, profile, channel_id, channel_name)
    }

    pub fn channel_permission(&self, channel_id: &str) -> ChannelPermission {
        self.channel_permissions
            .get(channel_id)
            .copied()
            .unwrap_or(self.default_channel_permission)
    }

    pub fn can_read_channel(&self, channel_id: &str) -> bool {
        self.channel_permission(channel_id).can_read()
    }

    pub fn can_write_channel(&self, channel_id: &str) -> bool {
        self.channel_permission(channel_id).can_write()
    }

    pub fn notification_rule_matches(
        &self,
        rule: &NotificationRule,
        item: &TimelineItem,
    ) -> Result<bool, SettingsError> {
        if !rule.matches(item)? {
            return Ok(false);
        }

        let synthetic_profile = SearchProfile {
            id: String::new(),
            label: String::new(),
            query: None,
            keyword_profiles: rule.keyword_profile_ids.clone(),
            section_profiles: rule.section_profile_ids.clone(),
            channel_profiles: rule.channel_profile_ids.clone(),
            author_profiles: rule.author_profile_ids.clone(),
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
        };

        if !rule.keyword_profile_ids.is_empty()
            || !rule.section_profile_ids.is_empty()
            || !rule.channel_profile_ids.is_empty()
            || !rule.author_profile_ids.is_empty()
        {
            if !profile_channel_matches(self, &synthetic_profile, item)? {
                return Ok(false);
            }
            if !profile_author_matches(self, &synthetic_profile, item)? {
                return Ok(false);
            }
            if !profile_keyword_matches(self, &synthetic_profile, item)? {
                return Ok(false);
            }
        }

        if rule.search_profile_ids.is_empty() {
            return Ok(true);
        }

        for profile_id in &rule.search_profile_ids {
            if self.search_profile_matches(profile_id, item)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn search_profile_matches(
        &self,
        profile_id: &str,
        item: &TimelineItem,
    ) -> Result<bool, SettingsError> {
        let profile = self.search_profile(profile_id).ok_or_else(|| {
            SettingsError::MissingProfileReference {
                kind: "search_profile".to_string(),
                id: profile_id.to_string(),
            }
        })?;

        if !profile_channel_matches(self, profile, item)? {
            return Ok(false);
        }
        if !profile_author_matches(self, profile, item)? {
            return Ok(false);
        }
        if !profile_keyword_matches(self, profile, item)? {
            return Ok(false);
        }

        if let Some(query) = profile
            .query
            .as_deref()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            && !item_matches_text_query(item, query)
        {
            return Ok(false);
        }

        Ok(true)
    }
}

fn normalize_optional_id(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn item_matches_text_query(item: &TimelineItem, query: &str) -> bool {
    let needle = query.to_lowercase();
    item.body.to_lowercase().contains(&needle)
        || item.author_name.to_lowercase().contains(&needle)
        || item.channel_name.to_lowercase().contains(&needle)
}

fn profile_channel_matches(
    settings: &AppSettings,
    profile: &SearchProfile,
    item: &TimelineItem,
) -> Result<bool, SettingsError> {
    profile_channel_matches_candidate(settings, profile, &item.channel_id, &item.channel_name)
}

fn profile_channel_matches_candidate(
    settings: &AppSettings,
    profile: &SearchProfile,
    channel_id: &str,
    channel_name: &str,
) -> Result<bool, SettingsError> {
    let mut allow_channels = profile.channels.clone();
    let mut allow_name_matchers = Vec::new();
    let mut deny_channels = BTreeSet::new();
    let mut deny_name_matchers = Vec::new();

    for profile_id in &profile.section_profiles {
        let section_profile = settings
            .section_profiles
            .iter()
            .find(|candidate| candidate.id == *profile_id)
            .ok_or_else(|| SettingsError::MissingProfileReference {
                kind: "section_profile".to_string(),
                id: profile_id.clone(),
            })?;
        allow_channels.extend(section_profile.channels.iter().cloned());
        allow_name_matchers.extend(section_profile.channel_name_matchers.iter().cloned());
    }

    for profile_id in &profile.channel_profiles {
        let channel_profile = settings
            .channel_profiles
            .iter()
            .find(|candidate| candidate.id == *profile_id)
            .ok_or_else(|| SettingsError::MissingProfileReference {
                kind: "channel_profile".to_string(),
                id: profile_id.clone(),
            })?;
        match channel_profile.mode {
            ProfileMode::Allow => {
                allow_channels.extend(channel_profile.channels.iter().cloned());
                allow_name_matchers.extend(channel_profile.channel_name_matchers.iter().cloned());
            }
            ProfileMode::Deny => {
                deny_channels.extend(channel_profile.channels.iter().cloned());
                deny_name_matchers.extend(channel_profile.channel_name_matchers.iter().cloned());
            }
        }
    }

    if deny_channels.contains(channel_id)
        || deny_name_matchers
            .iter()
            .try_fold(false, |matched, matcher| {
                if matched {
                    Ok(true)
                } else {
                    channel_name_matches(matcher, channel_name)
                }
            })
            .map_err(|source| SettingsError::Matcher(source.to_string()))?
    {
        return Ok(false);
    }

    if allow_channels.is_empty() && allow_name_matchers.is_empty() {
        return Ok(true);
    }

    if allow_channels.contains(channel_id) {
        return Ok(true);
    }

    allow_name_matchers
        .iter()
        .try_fold(false, |matched, matcher| {
            if matched {
                Ok(true)
            } else {
                channel_name_matches(matcher, channel_name)
            }
        })
        .map_err(|source| SettingsError::Matcher(source.to_string()))
}

fn channel_name_matches(
    matcher: &PatternMatcher,
    channel_name: &str,
) -> Result<bool, regex::Error> {
    let raw = channel_name.trim_start_matches('#');
    if matcher.matches(channel_name)? {
        Ok(true)
    } else if raw == channel_name {
        Ok(false)
    } else {
        matcher.matches(raw)
    }
}

fn profile_author_matches(
    settings: &AppSettings,
    profile: &SearchProfile,
    item: &TimelineItem,
) -> Result<bool, SettingsError> {
    let mut allow_authors = profile.authors.clone();
    let mut deny_authors = BTreeSet::new();

    for profile_id in &profile.author_profiles {
        let author_profile = settings
            .author_profiles
            .iter()
            .find(|candidate| candidate.id == *profile_id)
            .ok_or_else(|| SettingsError::MissingProfileReference {
                kind: "author_profile".to_string(),
                id: profile_id.clone(),
            })?;
        match author_profile.mode {
            ProfileMode::Allow => allow_authors.extend(author_profile.authors.iter().cloned()),
            ProfileMode::Deny => deny_authors.extend(author_profile.authors.iter().cloned()),
        }
    }

    if deny_authors.contains(&item.author_id) {
        return Ok(false);
    }

    Ok(allow_authors.is_empty() || allow_authors.contains(&item.author_id))
}

fn profile_keyword_matches(
    settings: &AppSettings,
    profile: &SearchProfile,
    item: &TimelineItem,
) -> Result<bool, SettingsError> {
    let mut has_allow_profiles = false;
    let mut allow_matched = false;

    for profile_id in &profile.keyword_profiles {
        let keyword_profile = settings
            .keyword_profiles
            .iter()
            .find(|candidate| candidate.id == *profile_id)
            .ok_or_else(|| SettingsError::MissingProfileReference {
                kind: "keyword_profile".to_string(),
                id: profile_id.clone(),
            })?;

        let matched = keyword_profile
            .matchers
            .iter()
            .try_fold(false, |matched, matcher| {
                if matched {
                    Ok(true)
                } else {
                    matcher.matches(&item.body)
                }
            })
            .map_err(|source| SettingsError::Matcher(source.to_string()))?;

        match keyword_profile.mode {
            ProfileMode::Allow => {
                has_allow_profiles = true;
                allow_matched |= matched;
            }
            ProfileMode::Deny => {
                if matched {
                    return Ok(false);
                }
            }
        }
    }

    if has_allow_profiles && !allow_matched {
        return Ok(false);
    }

    Ok(true)
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("invalid regex `{pattern}`")]
    InvalidRegex {
        pattern: String,
        #[source]
        source: regex::Error,
    },
    #[error("invalid quiet hours")]
    InvalidQuietHours,
    #[error("missing {kind} `{id}`")]
    MissingProfileReference { kind: String, id: String },
    #[error("matcher error: {0}")]
    Matcher(String),
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::{Duration, Utc};

    use crate::{
        models::{ReplyState, TimelineItem},
        settings::{
            AuthorProfile, ChannelPermission, ChannelProfile, KeywordProfile, NotificationAction,
            PatternMatcher, ProfileMode, SearchProfile, SectionProfile, ShortcutBindings,
        },
    };

    use super::{AppSettings, NotificationRule, SettingsError};

    fn sample_item() -> TimelineItem {
        TimelineItem {
            workspace_id: "W1".into(),
            channel_id: "C-eng".into(),
            channel_name: "eng".into(),
            message_ts: "1".into(),
            thread_ts: "1".into(),
            author_id: "U1".into(),
            author_name: "A".into(),
            author_avatar_path: None,
            body: "ready to ship this release".into(),
            reactions: vec![],
            attachments: vec![],
            unread: true,
            participant: false,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: 1,
            last_activity_at: Utc::now() - Duration::minutes(5),
            reply_state: ReplyState::Idle,
        }
    }

    #[test]
    fn invalid_regex_is_rejected() {
        let rule = NotificationRule {
            id: uuid::Uuid::new_v4(),
            label: "bad".into(),
            enabled: true,
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
            include: vec![PatternMatcher::Regex("[".into())],
            exclude: vec![],
            keyword_profile_ids: vec![],
            section_profile_ids: vec![],
            channel_profile_ids: vec![],
            author_profile_ids: vec![],
            search_profile_ids: vec![],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        };

        let error = rule.validate().unwrap_err();
        assert!(matches!(error, SettingsError::InvalidRegex { .. }));
    }

    #[test]
    fn rule_matches_text_filters() {
        let rule = NotificationRule {
            id: uuid::Uuid::new_v4(),
            label: "ship".into(),
            enabled: true,
            channels: BTreeSet::from(["C-eng".into()]),
            authors: BTreeSet::new(),
            include: vec![PatternMatcher::Text("ship".into())],
            exclude: vec![PatternMatcher::Text("wip".into())],
            keyword_profile_ids: vec![],
            section_profile_ids: vec![],
            channel_profile_ids: vec![],
            author_profile_ids: vec![],
            search_profile_ids: vec![],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        };

        assert!(rule.matches(&sample_item()).unwrap());
    }

    #[test]
    fn search_profile_reuses_small_profiles_with_allow_and_deny() {
        let mut settings = AppSettings::default();
        settings.keyword_profiles = vec![
            KeywordProfile {
                id: "shipping".into(),
                label: "Shipping".into(),
                mode: ProfileMode::Allow,
                matchers: vec![PatternMatcher::Text("ship".into())],
            },
            KeywordProfile {
                id: "noise".into(),
                label: "Noise".into(),
                mode: ProfileMode::Deny,
                matchers: vec![PatternMatcher::Text("wip".into())],
            },
        ];
        settings.channel_profiles = vec![
            ChannelProfile {
                id: "eng-only".into(),
                label: "Engineering".into(),
                mode: ProfileMode::Allow,
                channels: BTreeSet::from(["C-eng".into(), "C-release".into()]),
                channel_name_matchers: Vec::new(),
            },
            ChannelProfile {
                id: "exclude-random".into(),
                label: "Exclude random".into(),
                mode: ProfileMode::Deny,
                channels: BTreeSet::from(["C-random".into()]),
                channel_name_matchers: Vec::new(),
            },
        ];
        settings.section_profiles = vec![SectionProfile {
            id: "release-rooms".into(),
            label: "Release rooms".into(),
            channels: BTreeSet::from(["C-eng".into(), "C-release".into()]),
            channel_name_matchers: Vec::new(),
        }];
        settings.author_profiles = vec![AuthorProfile {
            id: "humans".into(),
            label: "Humans".into(),
            mode: ProfileMode::Allow,
            authors: BTreeSet::from(["U1".into()]),
        }];
        settings.search_profiles = vec![SearchProfile {
            id: "release-focus".into(),
            label: "Release focus".into(),
            query: None,
            keyword_profiles: vec!["shipping".into(), "noise".into()],
            section_profiles: vec!["release-rooms".into()],
            channel_profiles: vec!["eng-only".into(), "exclude-random".into()],
            author_profiles: vec!["humans".into()],
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
        }];

        assert!(
            settings
                .search_profile_matches("release-focus", &sample_item())
                .unwrap()
        );
    }

    #[test]
    fn notification_rules_can_reuse_search_profiles() {
        let mut settings = AppSettings::default();
        settings.keyword_profiles = vec![KeywordProfile {
            id: "shipping".into(),
            label: "Shipping".into(),
            mode: ProfileMode::Allow,
            matchers: vec![PatternMatcher::Text("ship".into())],
        }];
        settings.search_profiles = vec![SearchProfile {
            id: "release-focus".into(),
            label: "Release focus".into(),
            query: None,
            keyword_profiles: vec!["shipping".into()],
            section_profiles: vec![],
            channel_profiles: vec![],
            author_profiles: vec![],
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
        }];

        let rule = NotificationRule {
            id: uuid::Uuid::new_v4(),
            label: "Release profile".into(),
            enabled: true,
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
            include: vec![],
            exclude: vec![],
            keyword_profile_ids: vec![],
            section_profile_ids: vec![],
            channel_profile_ids: vec![],
            author_profile_ids: vec![],
            search_profile_ids: vec!["release-focus".into()],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        };

        assert!(
            settings
                .notification_rule_matches(&rule, &sample_item())
                .unwrap()
        );
    }

    #[test]
    fn notification_rules_can_reuse_profile_refs_without_inline_filters() {
        let mut settings = AppSettings::default();
        settings.keyword_profiles = vec![KeywordProfile {
            id: "shipping".into(),
            label: "Shipping".into(),
            mode: ProfileMode::Allow,
            matchers: vec![PatternMatcher::Text("ship".into())],
        }];
        settings.channel_profiles = vec![ChannelProfile {
            id: "eng-only".into(),
            label: "Engineering".into(),
            mode: ProfileMode::Allow,
            channels: BTreeSet::from(["C-eng".into()]),
            channel_name_matchers: Vec::new(),
        }];
        settings.author_profiles = vec![AuthorProfile {
            id: "humans".into(),
            label: "Humans".into(),
            mode: ProfileMode::Allow,
            authors: BTreeSet::from(["U1".into()]),
        }];

        let rule = NotificationRule {
            id: uuid::Uuid::new_v4(),
            label: "Profile refs".into(),
            enabled: true,
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
            include: vec![],
            exclude: vec![],
            keyword_profile_ids: vec!["shipping".into()],
            section_profile_ids: vec![],
            channel_profile_ids: vec!["eng-only".into()],
            author_profile_ids: vec!["humans".into()],
            search_profile_ids: vec![],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        };

        assert!(
            settings
                .notification_rule_matches(&rule, &sample_item())
                .unwrap()
        );
    }

    #[test]
    fn search_profile_can_match_channel_name_patterns() {
        let mut settings = AppSettings::default();
        settings.channel_profiles = vec![ChannelProfile {
            id: "times-only".into(),
            label: "Times only".into(),
            mode: ProfileMode::Allow,
            channels: BTreeSet::new(),
            channel_name_matchers: vec![PatternMatcher::Regex("^times_".into())],
        }];
        settings.search_profiles = vec![SearchProfile {
            id: "times".into(),
            label: "Times".into(),
            query: None,
            keyword_profiles: vec![],
            section_profiles: vec![],
            channel_profiles: vec!["times-only".into()],
            author_profiles: vec![],
            channels: BTreeSet::new(),
            authors: BTreeSet::new(),
        }];

        let mut item = sample_item();
        item.channel_id = "C-times".into();
        item.channel_name = "#times_alice".into();

        assert!(settings.search_profile_matches("times", &item).unwrap());
        assert!(
            settings
                .channel_matches_active_search_profile("C-times", "#times_alice")
                .unwrap_or(false)
                == false
        );

        settings.active_search_profile_id = Some("times".into());
        assert!(
            settings
                .channel_matches_active_search_profile("C-times", "#times_alice")
                .unwrap()
        );
    }

    #[test]
    fn channel_permissions_default_to_read_write() {
        let mut settings = AppSettings::default();
        settings.default_channel_permission = ChannelPermission::ReadOnly;
        settings
            .channel_permissions
            .insert("C-writable".into(), ChannelPermission::ReadWrite);

        assert!(settings.can_read_channel("C-eng"));
        assert!(!settings.can_write_channel("C-eng"));
        assert!(settings.can_write_channel("C-writable"));
    }

    #[test]
    fn apply_patch_overrides_only_requested_fields() {
        let mut settings = AppSettings::default();
        settings.timeline.focus_keywords = vec!["incident".into()];

        settings.apply_patch(super::AppSettingsPatch {
            slack: Some(super::SlackConfigSettings {
                app_token: Some("xapp-123".into()),
                ..Default::default()
            }),
            active_search_profile_id: Some("focus".into()),
            channel_permissions: Some(BTreeMap::from([(
                "C-eng".into(),
                ChannelPermission::ReadOnly,
            )])),
            ..Default::default()
        });

        assert_eq!(settings.active_search_profile_id.as_deref(), Some("focus"));
        assert_eq!(
            settings.channel_permission("C-eng"),
            ChannelPermission::ReadOnly
        );
        assert_eq!(settings.slack.app_token.as_deref(), Some("xapp-123"));
        assert_eq!(
            settings.timeline.focus_keywords,
            vec!["incident".to_string()]
        );
    }

    #[test]
    fn shortcut_bindings_have_expected_defaults() {
        let shortcuts = ShortcutBindings::default();

        assert_eq!(shortcuts.open_settings, vec!["<Ctrl>comma".to_string()]);
        assert_eq!(shortcuts.open_admin, vec!["<Ctrl><Shift>a".to_string()]);
        assert_eq!(shortcuts.focus_search, vec!["<Ctrl>k".to_string()]);
        assert_eq!(shortcuts.focus_composer, vec!["<Ctrl>n".to_string()]);
        assert_eq!(
            shortcuts.close_column,
            vec!["<Ctrl>w".to_string(), "Escape".to_string()]
        );
    }
}
