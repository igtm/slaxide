use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    File,
    Image,
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentSummary {
    pub kind: AttachmentKind,
    pub title: String,
    pub url: Option<String>,
    pub mime: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyState {
    Idle,
    Drafting,
    Sending,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingReason {
    DirectMention,
    FocusKeyword(String),
    ParticipatingThread,
    WeightedWatchedChannel(u8),
    RecentActivity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineItem {
    pub workspace_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub message_ts: String,
    pub thread_ts: String,
    pub author_id: String,
    pub author_name: String,
    pub body: String,
    pub attachments: Vec<AttachmentSummary>,
    pub unread: bool,
    pub participant: bool,
    pub direct_mention: bool,
    pub focus_keyword_hits: Vec<String>,
    pub watch_weight: u8,
    pub last_activity_at: DateTime<Utc>,
    pub reply_state: ReplyState,
}

impl TimelineItem {
    pub fn age_minutes(&self, now: DateTime<Utc>) -> i64 {
        (now - self.last_activity_at).num_minutes().max(0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RankedTimelineItem {
    pub item: TimelineItem,
    pub score: f64,
    pub reasons: Vec<RankingReason>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineMode {
    Focus,
    Recent,
}
