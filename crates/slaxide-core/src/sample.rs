use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::{
    AppSettings, AttachmentKind, AttachmentSummary, NotificationAction, NotificationRule,
    PatternMatcher, QuietHours, ReplyState, ThemeId, TimelineItem, TimelinePolicy,
};

pub fn sample_settings() -> AppSettings {
    AppSettings {
        theme_id: ThemeId::TokyoNightStorm,
        timeline: TimelinePolicy {
            watched_channels: BTreeSet::from([
                "C-eng".into(),
                "C-release".into(),
                "C-incident".into(),
            ]),
            muted_channels: BTreeSet::from(["C-random".into()]),
            channel_weights: BTreeMap::from([
                ("C-eng".into(), 2),
                ("C-release".into(), 3),
                ("C-incident".into(), 4),
            ]),
            focus_keywords: vec!["ship".into(), "incident".into(), "prod".into()],
            focus_threshold: 75.0,
            recent_window_days: 7,
        },
        notification_rules: vec![
            NotificationRule {
                id: Uuid::new_v4(),
                label: "Incidents".into(),
                enabled: true,
                channels: BTreeSet::from(["C-incident".into()]),
                authors: BTreeSet::new(),
                include: vec![PatternMatcher::Regex("SEV[12]".into())],
                exclude: vec![],
                thread_participation_only: false,
                quiet_hours: None,
                action: NotificationAction::Critical,
            },
            NotificationRule {
                id: Uuid::new_v4(),
                label: "Ship room".into(),
                enabled: true,
                channels: BTreeSet::from(["C-release".into()]),
                authors: BTreeSet::new(),
                include: vec![PatternMatcher::Text("ship".into())],
                exclude: vec![PatternMatcher::Text("wip".into())],
                thread_participation_only: false,
                quiet_hours: Some(QuietHours {
                    start_hour: 23,
                    end_hour: 7,
                }),
                action: NotificationAction::Notify,
            },
        ],
    }
}

pub fn sample_timeline() -> Vec<TimelineItem> {
    let now = Utc::now();

    vec![
        TimelineItem {
            workspace_id: "W-dev".into(),
            channel_id: "C-incident".into(),
            channel_name: "#incident".into(),
            message_ts: "1742010000.001".into(),
            thread_ts: "1742010000.001".into(),
            author_id: "U-ops".into(),
            author_name: "ops-bot".into(),
            body: "SEV1: prod API latency spike on checkout path".into(),
            attachments: vec![AttachmentSummary {
                kind: AttachmentKind::Link,
                title: "Grafana dashboard".into(),
                url: Some("https://grafana.example.internal".into()),
                mime: None,
            }],
            unread: true,
            participant: true,
            direct_mention: true,
            focus_keyword_hits: vec!["incident".into(), "prod".into()],
            watch_weight: 4,
            last_activity_at: now - Duration::minutes(4),
            reply_state: ReplyState::Idle,
        },
        TimelineItem {
            workspace_id: "W-dev".into(),
            channel_id: "C-release".into(),
            channel_name: "#release".into(),
            message_ts: "1742009000.001".into(),
            thread_ts: "1742009000.001".into(),
            author_id: "U-pm".into(),
            author_name: "release-pm".into(),
            body: "Can we ship the fix today if the smoke tests stay green?".into(),
            attachments: vec![],
            unread: true,
            participant: false,
            direct_mention: false,
            focus_keyword_hits: vec!["ship".into()],
            watch_weight: 3,
            last_activity_at: now - Duration::minutes(9),
            reply_state: ReplyState::Idle,
        },
        TimelineItem {
            workspace_id: "W-dev".into(),
            channel_id: "C-eng".into(),
            channel_name: "#eng".into(),
            message_ts: "1742007000.001".into(),
            thread_ts: "1742006000.001".into(),
            author_id: "U-lead".into(),
            author_name: "eng-lead".into(),
            body: "Posting thread summary: branch is stable, waiting on one final review.".into(),
            attachments: vec![AttachmentSummary {
                kind: AttachmentKind::File,
                title: "release-checklist.md".into(),
                url: None,
                mime: Some("text/markdown".into()),
            }],
            unread: true,
            participant: true,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: 2,
            last_activity_at: now - Duration::minutes(18),
            reply_state: ReplyState::Idle,
        },
        TimelineItem {
            workspace_id: "W-dev".into(),
            channel_id: "C-random".into(),
            channel_name: "#random".into(),
            message_ts: "1741999000.001".into(),
            thread_ts: "1741999000.001".into(),
            author_id: "U-fun".into(),
            author_name: "teammate".into(),
            body: "Lunch poll time".into(),
            attachments: vec![],
            unread: true,
            participant: false,
            direct_mention: false,
            focus_keyword_hits: vec![],
            watch_weight: 1,
            last_activity_at: now - Duration::hours(1),
            reply_state: ReplyState::Idle,
        },
    ]
}
