//! The transactional outbox: write the event inside the domain transaction.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{partition_default, OpsError, Result};

/// One fact, ready to be written to the outbox.
#[derive(Debug, Clone)]
pub struct Event {
    /// The event's identity. On the consuming side it becomes the job id, which is what
    /// makes a duplicate delivery a no-op rather than a double count.
    pub id: Uuid,
    /// `<service>.<aggregate>.<past-tense fact>.v<major>`.
    pub event_type: String,
    /// `<aggregate>:<id>`. The partition is derived from this.
    pub subject: String,
    /// The event body, stored as jsonb.
    pub payload: Value,
    /// When the fact became true. Defaults to now when absent.
    pub time: Option<DateTime<Utc>>,
}

impl Event {
    /// Builds an event with a fresh id and the current time.
    #[must_use]
    pub fn new(event_type: impl Into<String>, subject: impl Into<String>, payload: Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type: event_type.into(),
            subject: subject.into(),
            payload,
            time: Some(Utc::now()),
        }
    }

    /// Rejects an event that cannot be stored meaningfully.
    fn validate(&self) -> Result<()> {
        if self.id == Uuid::nil() {
            return Err(OpsError::Invalid("an event needs an id".into()));
        }
        if self.event_type.is_empty() {
            return Err(OpsError::Invalid("an event needs a type".into()));
        }
        if self.subject.is_empty() {
            return Err(OpsError::Invalid("an event needs a subject".into()));
        }
        Ok(())
    }
}

/// An event as stored: the caller's event plus the sequence number the database
/// assigned.
#[derive(Debug, Clone)]
pub struct Enqueued {
    pub event: Event,
    /// Monotonic per database and **not contiguous**. A cursor, never a count.
    pub seq: i64,
    pub partition: i16,
}

impl Enqueued {
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.event.id
    }
}

/// Writes an event inside the caller's transaction.
///
/// It takes a connection that is already in a transaction on purpose: the promise of the
/// transactional outbox is that the event and the domain change commit together, so the
/// signature makes the wrong thing impossible.
///
/// The partition is computed here from the subject, so no caller can choose one and
/// silently break ordering for its aggregate.
pub async fn enqueue(conn: &mut PgConnection, event: &Event) -> Result<Enqueued> {
    event.validate()?;
    let partition = partition_default(&event.subject);
    let time = event.time.unwrap_or_else(Utc::now);

    let row: (i64,) = sqlx::query_as(
        "INSERT INTO ops.outbox_event (id, type, subject, partition, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING seq",
    )
    .bind(event.id)
    .bind(&event.event_type)
    .bind(&event.subject)
    .bind(i32::from(partition))
    .bind(&event.payload)
    .bind(time)
    .fetch_one(&mut *conn)
    .await?;

    Ok(Enqueued {
        event: Event {
            time: Some(time),
            ..event.clone()
        },
        seq: row.0,
        partition: partition as i16,
    })
}
