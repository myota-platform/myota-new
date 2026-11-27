//! The read side: the event feed a peer pulls, its acknowledgement, and retention.

use std::collections::HashMap;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::Row;

use crate::{store::Store, Enqueued, Event, Result};

/// One batch of the event log, as `GET /internal/events` returns it.
#[derive(Debug, Clone)]
pub struct Feed {
    pub events: Vec<Enqueued>,
    /// The cursor to pass as `after`. Advances even when `events` is empty, so a consumer
    /// filtering by type does not re-read the same window forever.
    pub next_seq: i64,
    /// Whether rows exist past `next_seq`.
    pub more: bool,
}

impl Store {
    /// Returns published events with `seq > after`, in seq order.
    ///
    /// Only published rows: `published_at IS NOT NULL` is the relay's high-water mark in
    /// practice, and it is what stops a consumer reading past an event that committed
    /// late.
    pub async fn read_events(&self, after: i64, limit: i64, types: &[String]) -> Result<Feed> {
        let limit = if limit <= 0 || limit > 500 {
            100
        } else {
            limit
        };
        let after = after.max(0);

        // One row more than asked for, so `more` is known without a second query.
        // `coalesce(cardinality(...), 0)`: an empty filter would otherwise arrive as NULL
        // and make the whole predicate NULL, filtering out every row.
        let rows = sqlx::query(
            "SELECT seq, id, type, subject, partition, payload, created_at \
             FROM ops.outbox_event \
             WHERE seq > $1 AND published_at IS NOT NULL \
               AND (coalesce(cardinality($3::text[]), 0) = 0 OR type = ANY($3)) \
             ORDER BY seq LIMIT $2 + 1",
        )
        .bind(after)
        .bind(limit)
        .bind(types)
        .fetch_all(&self.pool)
        .await?;

        let mut feed = Feed {
            events: Vec::with_capacity(rows.len()),
            next_seq: after,
            more: false,
        };
        for row in rows {
            if feed.events.len() as i64 == limit {
                feed.more = true;
                break;
            }
            let seq: i64 = row.get("seq");
            feed.events.push(Enqueued {
                seq,
                partition: row.get::<i32, _>("partition") as i16,
                event: Event {
                    id: row.get("id"),
                    event_type: row.get("type"),
                    subject: row.get("subject"),
                    payload: row.get("payload"),
                    time: Some(row.get::<DateTime<Utc>, _>("created_at")),
                },
            });
            feed.next_seq = seq;
        }
        Ok(feed)
    }

    /// Records how far a consumer has durably applied, so the producer can trim without
    /// trimming past a live consumer.
    ///
    /// Monotonic: a lower seq is ignored rather than applied, because a replica that has
    /// fallen behind must not move the floor backwards and un-protect events another
    /// consumer still needs.
    pub async fn ack_events(&self, consumer: &str, source: &str, seq: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO ops.event_ack (consumer, source, last_acked_seq, reported_at) \
             VALUES ($1, $2, $3, now()) \
             ON CONFLICT (consumer, source) DO UPDATE \
             SET last_acked_seq = GREATEST(ops.event_ack.last_acked_seq, EXCLUDED.last_acked_seq), \
                 reported_at = now()",
        )
        .bind(consumer)
        .bind(source)
        .bind(seq)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Deletes published events the system no longer needs, with two independent bounds.
    ///
    /// The age ceiling bounds storage even if a consumer is gone; the ack floor never
    /// trims past what a live consumer has confirmed. Either alone would be wrong.
    pub async fn trim(&self, retention: ChronoDuration, keep_recent: i64) -> Result<u64> {
        let retention = if retention <= ChronoDuration::zero() {
            ChronoDuration::days(14)
        } else {
            retention
        };
        let keep_recent = keep_recent.max(0);

        let result = sqlx::query(
            "DELETE FROM ops.outbox_event \
             WHERE published_at IS NOT NULL \
               AND created_at < now() - make_interval(secs => $1) \
               AND seq < COALESCE((SELECT min(last_acked_seq) FROM ops.event_ack), 0) \
               AND seq < (SELECT max(seq) FROM ops.outbox_event) - $2",
        )
        .bind(retention.num_seconds() as f64)
        .bind(keep_recent)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Reports, per consumer and source, how far behind the newest event the consumer's
    /// acknowledgement is. This is the metric that turns "a consumer is stuck" into an
    /// alert rather than a discovery.
    pub async fn event_lag(&self) -> Result<HashMap<String, ChronoDuration>> {
        let rows = sqlx::query(
            "SELECT a.consumer, a.source, now() - (SELECT created_at FROM ops.outbox_event \
             WHERE seq > a.last_acked_seq ORDER BY seq LIMIT 1) AS lag \
             FROM ops.event_ack a",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut lags = HashMap::with_capacity(rows.len());
        for row in rows {
            let consumer: String = row.get("consumer");
            let source: String = row.get("source");
            if let Ok(lag) = row.try_get::<DateTime<Utc>, _>("lag") {
                lags.insert(format!("{consumer}/{source}"), Utc::now() - lag);
            }
        }
        Ok(lags)
    }
}
