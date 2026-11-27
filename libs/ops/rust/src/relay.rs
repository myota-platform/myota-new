//! The relay: publishes outbox events, one partition at a time, under a lease.
//!
//! ## Why a relay at all, when there is no broker
//!
//! ADR-0004 removed the broker: the outbox row *is* the log, and consumers pull it over
//! HTTP. So "publishing" is not a network operation — it is setting `published_at`,
//! which makes an event visible on the feed.
//!
//! That sounds like a column, but it fixes a real ordering problem. A `bigserial` is
//! allocated at INSERT time, not at COMMIT time, so two concurrent transactions can
//! commit out of sequence order: A takes seq 1, B takes seq 2, and B commits first. A
//! consumer reading the feed at that moment sees seq 2 and not seq 1; if it advanced its
//! cursor, seq 1 would be lost silently.
//!
//! The relay publishes in ascending seq order under a lease, so a feed reader that stops
//! at the first gap is correct: a gap below the high-water mark is a trimmed row (already
//! acked), and a gap above it is simply not published yet.
//!
//! ## What is deliberately not solved
//!
//! If A holds seq 1 uncommitted for a long time while B commits seq 2, the relay sees
//! only seq 2 and publishes it. Closing that needs transaction-id bookkeeping or a
//! settle delay, and both cost more than the problem currently does. Recorded as a
//! finding rather than papered over.
//!
//! ## Concurrency
//!
//! A partition lease gives one relay per partition, so ordering holds when several
//! replicas run. The `published_at IS NULL` predicate in the update is a second,
//! independent guard: even if two relays raced on one partition, a row is published once.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{store::Store, Enqueued, Result, PARTITIONS};

/// A claimed partition and how long the claim is good for.
#[derive(Debug, Clone)]
pub struct PartitionLease {
    pub partition: i16,
    pub owner: String,
    pub expires_at: DateTime<Utc>,
    /// The highest seq already published for this partition.
    pub last_seq: i64,
}

/// The result of one publish pass.
#[derive(Debug, Clone, Default)]
pub struct RelayBatch {
    /// The events made visible by this pass, in seq order.
    pub published: Vec<Enqueued>,
    pub partition: i16,
    /// Whether the partition had more unpublished rows than the batch allowed.
    pub more: bool,
}

impl Store {
    /// Takes the lease on one free partition.
    ///
    /// `None` when every partition is leased to someone else — the normal steady state
    /// with [`PARTITIONS`] replicas running, not an error.
    pub async fn claim_partition(&self) -> Result<Option<PartitionLease>> {
        self.ensure_partition_rows().await?;

        let row = sqlx::query(
            "UPDATE ops.outbox_partition \
             SET lease_owner = $1, lease_until = $2, updated_at = now() \
             WHERE partition = ( \
                 SELECT partition FROM ops.outbox_partition \
                 WHERE lease_until IS NULL OR lease_until < now() \
                 -- Least recently worked, not lowest number: ordering by id would make \
                 -- every relay claim partition 0, release it, and claim it again, so the \
                 -- higher partitions would starve. \
                 ORDER BY updated_at, partition \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT 1) \
             RETURNING partition, lease_owner, lease_until, last_seq",
        )
        .bind(&self.owner)
        .bind(self.lease_deadline())
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|row| PartitionLease {
            partition: row.get::<i16, _>("partition"),
            owner: row.get::<String, _>("lease_owner"),
            expires_at: row.get::<DateTime<Utc>, _>("lease_until"),
            last_seq: row.get::<i64, _>("last_seq"),
        }))
    }

    async fn ensure_partition_rows(&self) -> Result<()> {
        sqlx::query(
            "INSERT INTO ops.outbox_partition (partition) \
             SELECT generate_series(0, $1 - 1) ON CONFLICT (partition) DO NOTHING",
        )
        .bind(i32::from(PARTITIONS))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Publishes up to `limit` unpublished events for one partition, in seq order, and
    /// advances the partition's high-water mark.
    pub async fn publish_partition(&self, partition: i16, limit: i64) -> Result<RelayBatch> {
        let limit = if limit <= 0 { 100 } else { limit };

        // A CTE rather than a bare UPDATE: `UPDATE … RETURNING` has no ORDER BY, and the
        // picking step needs `FOR UPDATE SKIP LOCKED` so two relays that somehow hold one
        // partition still take disjoint rows. The `published_at IS NULL` predicate is
        // repeated in the update so a row another relay published between the pick and the
        // write is skipped rather than published twice.
        let rows = sqlx::query(
            "WITH picked AS ( \
                 SELECT seq FROM ops.outbox_event \
                 WHERE partition = $1 AND published_at IS NULL \
                 ORDER BY seq LIMIT $2 \
                 FOR UPDATE SKIP LOCKED \
             ), updated AS ( \
                 UPDATE ops.outbox_event AS e SET published_at = now() \
                 FROM picked \
                 WHERE e.seq = picked.seq AND e.published_at IS NULL \
                 RETURNING e.seq, e.id, e.type, e.subject, e.partition, e.payload, e.created_at \
             ) \
             SELECT seq, id, type, subject, partition, payload, created_at \
             FROM updated ORDER BY seq",
        )
        .bind(i32::from(partition))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let mut batch = RelayBatch {
            partition,
            ..RelayBatch::default()
        };
        for row in rows {
            batch.published.push(Enqueued {
                seq: row.get("seq"),
                partition: row.get::<i32, _>("partition") as i16,
                event: crate::Event {
                    id: row.get("id"),
                    event_type: row.get("type"),
                    subject: row.get("subject"),
                    payload: row.get("payload"),
                    time: Some(row.get("created_at")),
                },
            });
        }

        let Some(last) = batch.published.last() else {
            return Ok(batch);
        };
        let last_seq = last.seq;

        // GREATEST rather than assignment: a relay that publishes an older row after a
        // newer one must not move the mark back and make a consumer re-read.
        sqlx::query(
            "UPDATE ops.outbox_partition \
             SET last_seq = GREATEST(last_seq, $2), updated_at = now() WHERE partition = $1",
        )
        .bind(i32::from(partition))
        .bind(last_seq)
        .execute(&self.pool)
        .await?;

        let (more,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM ops.outbox_event \
             WHERE partition = $1 AND published_at IS NULL AND seq > $2)",
        )
        .bind(i32::from(partition))
        .bind(last_seq)
        .fetch_one(&self.pool)
        .await?;
        batch.more = more;
        Ok(batch)
    }

    /// Gives up a lease early. Not required — leases expire — but it makes the partition
    /// available to another replica immediately.
    pub async fn release_partition(&self, partition: i16) -> Result<()> {
        sqlx::query(
            "UPDATE ops.outbox_partition \
             SET lease_owner = NULL, lease_until = NULL, updated_at = now() \
             WHERE partition = $1 AND lease_owner = $2",
        )
        .bind(i32::from(partition))
        .bind(&self.owner)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims one partition and publishes one batch, returning how many events became
    /// visible (0 when no partition was free).
    pub async fn relay_once(&self, limit: i64) -> Result<usize> {
        let Some(lease) = self.claim_partition().await? else {
            return Ok(0);
        };
        let batch = self.publish_partition(lease.partition, limit).await;
        let _ = self.release_partition(lease.partition).await;
        Ok(batch?.published.len())
    }

    /// Publishes everything currently unpublished, then returns.
    ///
    /// Walks every partition rather than using `claim_partition`, because a single claim
    /// can return a partition with no work while another still has rows — and stopping
    /// there would make the drain silently incomplete.
    pub async fn relay_drain(&self, limit: i64) -> Result<usize> {
        let mut total = 0;
        loop {
            let mut progressed = false;
            for partition in 0..PARTITIONS as i16 {
                let published = self.drain_partition(partition, limit).await?;
                if published > 0 {
                    progressed = true;
                    total += published;
                }
            }
            if !progressed {
                return Ok(total);
            }
        }
    }

    async fn drain_partition(&self, partition: i16, limit: i64) -> Result<usize> {
        self.ensure_partition_rows().await?;

        let claimed = sqlx::query(
            "UPDATE ops.outbox_partition \
             SET lease_owner = $1, lease_until = $2, updated_at = now() \
             WHERE partition = $3 AND (lease_until IS NULL OR lease_until < now()) \
             RETURNING partition",
        )
        .bind(&self.owner)
        .bind(self.lease_deadline())
        .bind(i32::from(partition))
        .fetch_optional(&self.pool)
        .await?;

        if claimed.is_none() {
            return Ok(0);
        }

        let batch = self.publish_partition(partition, limit).await;
        let _ = self.release_partition(partition).await;
        Ok(batch?.published.len())
    }
}
