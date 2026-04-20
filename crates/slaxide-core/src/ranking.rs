use chrono::{DateTime, Utc};

use crate::{
    models::{RankedTimelineItem, RankingReason, TimelineItem, TimelineMode},
    settings::TimelinePolicy,
};

#[derive(Clone, Copy, Debug)]
pub struct TimelineRanker {
    now: DateTime<Utc>,
}

impl Default for TimelineRanker {
    fn default() -> Self {
        Self { now: Utc::now() }
    }
}

impl TimelineRanker {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self { now }
    }

    pub fn rank(
        &self,
        policy: &TimelinePolicy,
        items: impl IntoIterator<Item = TimelineItem>,
    ) -> Vec<RankedTimelineItem> {
        let mut ranked = items
            .into_iter()
            .filter_map(|item| self.score_item(policy, item))
            .collect::<Vec<_>>();

        ranked.sort_by(|left, right| {
            right
                .item
                .last_activity_at
                .cmp(&left.item.last_activity_at)
                .then_with(|| right.score.total_cmp(&left.score))
        });
        ranked
    }

    pub fn visible_items(
        &self,
        mode: TimelineMode,
        policy: &TimelinePolicy,
        items: impl IntoIterator<Item = TimelineItem>,
    ) -> Vec<RankedTimelineItem> {
        self.rank(policy, items)
            .into_iter()
            .filter(|ranked| match mode {
                TimelineMode::Focus => {
                    !policy.muted_channels.contains(&ranked.item.channel_id)
                        && ranked.score >= policy.focus_threshold
                }
                TimelineMode::Recent => policy.is_effectively_watched(&ranked.item.channel_id),
            })
            .collect()
    }

    fn score_item(
        &self,
        policy: &TimelinePolicy,
        item: TimelineItem,
    ) -> Option<RankedTimelineItem> {
        let watched = policy.is_effectively_watched(&item.channel_id);
        let important = item.direct_mention
            || item.participant
            || !item.focus_keyword_hits.is_empty()
            || !policy.matching_keywords(&item.body).is_empty();

        if !watched && !important {
            return None;
        }

        let mut reasons = Vec::new();
        let mut score = 0.0;

        if item.direct_mention {
            score += 100.0;
            reasons.push(RankingReason::DirectMention);
        }

        for keyword in item
            .focus_keyword_hits
            .iter()
            .cloned()
            .chain(policy.matching_keywords(&item.body))
        {
            score += 25.0;
            reasons.push(RankingReason::FocusKeyword(keyword));
        }

        if item.participant && item.unread {
            score += 70.0;
            reasons.push(RankingReason::ParticipatingThread);
        }

        if watched {
            let weight = policy.weight_for(&item.channel_id).max(item.watch_weight);
            score += 20.0 + (f64::from(weight) * 10.0) + if item.unread { 10.0 } else { 0.0 };
            reasons.push(RankingReason::WeightedWatchedChannel(weight));
        }

        if item.unread {
            score += 5.0;
            reasons.push(RankingReason::RecentActivity);
        }

        let decay = 240.0 / (f64::from(item.age_minutes(self.now) as i32) + 30.0);
        score += decay.min(12.0);

        if policy.muted_channels.contains(&item.channel_id) {
            score -= 1_000.0;
        }

        Some(RankedTimelineItem {
            item,
            score,
            reasons,
        })
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use crate::{
        TimelineMode,
        sample::{sample_settings, sample_timeline},
    };

    use super::TimelineRanker;

    #[test]
    fn direct_mentions_rank_above_participating_threads() {
        let settings = sample_settings();
        let items = sample_timeline();
        let now = Utc.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap();
        let ranker = TimelineRanker::new(now);

        let ranked = ranker.visible_items(TimelineMode::Focus, &settings.timeline, items);

        assert!(ranked.len() >= 2);
        assert!(ranked[0].item.direct_mention);
        assert!(
            ranked[0].score > ranked[1].score,
            "top ranked item should outscore the next focus item"
        );
    }

    #[test]
    fn recent_mode_keeps_watched_channels_even_when_below_focus_threshold() {
        let mut settings = sample_settings();
        settings.timeline.focus_threshold = 999.0;
        let now = Utc::now() + Duration::minutes(5);
        let ranker = TimelineRanker::new(now);

        let recent =
            ranker.visible_items(TimelineMode::Recent, &settings.timeline, sample_timeline());
        let focus =
            ranker.visible_items(TimelineMode::Focus, &settings.timeline, sample_timeline());

        assert!(!recent.is_empty());
        assert!(focus.is_empty());
    }

    #[test]
    fn recent_mode_falls_back_to_all_channels_when_watch_list_is_empty() {
        let mut settings = sample_settings();
        settings.timeline.watched_channels.clear();
        settings.timeline.channel_weights.clear();
        let now = Utc::now() + Duration::minutes(5);
        let ranker = TimelineRanker::new(now);

        let recent =
            ranker.visible_items(TimelineMode::Recent, &settings.timeline, sample_timeline());

        assert!(!recent.is_empty());
        assert!(
            recent.iter().any(|item| item.item.channel_id == "C-random"),
            "recent should not go empty just because watched_channels is unset"
        );
    }

    #[test]
    fn visible_items_are_sorted_newest_first() {
        let settings = sample_settings();
        let now = Utc::now() + Duration::minutes(5);
        let ranker = TimelineRanker::new(now);

        let ranked =
            ranker.visible_items(TimelineMode::Recent, &settings.timeline, sample_timeline());

        assert!(ranked.len() >= 2);
        assert!(
            ranked
                .windows(2)
                .all(|pair| pair[0].item.last_activity_at >= pair[1].item.last_activity_at)
        );
    }
}
