//! Trait for Lance (or other) event-log backends.
//!
//! The async Lance dataset implementation currently lives in `persisting-engine`.
//! This trait is the pChronicle-owned contract for future in-crate Lance support
//! (`lance-store` feature).

use crate::formats::events::EventRecord;
use crate::Result;

/// Append-only / read API for `{run}/events.lance`-shaped stores.
pub trait EventLogStore: Send {
    fn append(&mut self, records: &[EventRecord]) -> Result<usize>;
    fn read_all(&self) -> Result<Vec<EventRecord>>;
    fn len(&self) -> Result<usize>;
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}
