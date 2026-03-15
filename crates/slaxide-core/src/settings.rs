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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRule {
    pub id: Uuid,
    pub label: String,
    pub enabled: bool,
    pub channels: BTreeSet<String>,
    pub authors: BTreeSet<String>,
    pub include: Vec<PatternMatcher>,
    pub exclude: Vec<PatternMatcher>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimelinePolicy {
    pub watched_channels: BTreeSet<String>,
    pub muted_channels: BTreeSet<String>,
    pub channel_weights: BTreeMap<String, u8>,
    pub focus_keywords: Vec<String>,
    pub focus_threshold: f64,
    pub recent_window_days: u16,
}

impl TimelinePolicy {
    pub fn is_watched(&self, channel_id: &str) -> bool {
        self.watched_channels.contains(channel_id)
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub theme_id: ThemeId,
    pub timeline: TimelinePolicy,
    pub notification_rules: Vec<NotificationRule>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme_id: ThemeId::TokyoNightStorm,
            timeline: TimelinePolicy {
                watched_channels: BTreeSet::new(),
                muted_channels: BTreeSet::new(),
                channel_weights: BTreeMap::new(),
                focus_keywords: Vec::new(),
                focus_threshold: 75.0,
                recent_window_days: 7,
            },
            notification_rules: Vec::new(),
        }
    }
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
    #[error("matcher error: {0}")]
    Matcher(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{Duration, Utc};

    use crate::{
        models::{ReplyState, TimelineItem},
        settings::{NotificationAction, PatternMatcher},
    };

    use super::{NotificationRule, SettingsError};

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
            label: "focus".into(),
            enabled: true,
            channels: BTreeSet::from(["C-eng".into()]),
            authors: BTreeSet::new(),
            include: vec![PatternMatcher::Text("ship".into())],
            exclude: vec![PatternMatcher::Text("wip".into())],
            thread_participation_only: false,
            quiet_hours: None,
            action: NotificationAction::Notify,
        };

        let item = TimelineItem {
            workspace_id: "W1".into(),
            channel_id: "C-eng".into(),
            channel_name: "eng".into(),
            message_ts: "1".into(),
            thread_ts: "1".into(),
            author_id: "U1".into(),
            author_name: "A".into(),
            body: "ready to ship this release".into(),
            attachments: vec![],
            unread: true,
            participant: false,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: 1,
            last_activity_at: Utc::now() - Duration::minutes(5),
            reply_state: ReplyState::Idle,
        };

        assert!(rule.matches(&item).unwrap());
    }
}
