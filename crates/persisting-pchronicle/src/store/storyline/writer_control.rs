use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) use super::super::cas_store::unix_now_ms;
use super::super::opendal_store::{self, Version};
use super::{
    CURRENT_FILE, StorylineLanceStore, StorylineSnapshotPointer, validate_current_control,
    write_local_current,
};

const CONTROL_CAS_RETRIES: usize = 32;
pub(super) const WRITER_LEASE_TTL_MS: u64 = 60_000;
pub(super) const CURRENT_CONTROL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StorylineCurrentControl {
    pub(super) control_version: u32,
    pub(super) revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) committed: Option<StorylineSnapshotPointer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) lease: Option<StorylineWriterLease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct StorylineWriterLease {
    pub(super) epoch: u64,
    pub(super) owner_id: String,
    pub(super) issued_at_unix_ms: u64,
    pub(super) expires_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) base_generation: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct CurrentControlState {
    pub(super) control: StorylineCurrentControl,
    pub(super) version: Option<Version>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcquiredLease {
    pub(super) lease: StorylineWriterLease,
    pub(super) takeover: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LeaseAcquireOutcome {
    Acquired(AcquiredLease),
    Held(StorylineWriterLease),
}

pub(super) struct WriterLeaseRenewal {
    lost: Arc<AtomicBool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl WriterLeaseRenewal {
    pub(super) async fn stop(mut self) -> bool {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        !self.lost.load(Ordering::Acquire)
    }
}

impl Drop for WriterLeaseRenewal {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

pub(super) fn empty_control() -> StorylineCurrentControl {
    StorylineCurrentControl {
        control_version: CURRENT_CONTROL_VERSION,
        revision: 0,
        committed: None,
        lease: None,
    }
}

pub(super) fn decode_control(contents: &str) -> Result<StorylineCurrentControl> {
    let value = serde_json::from_str::<serde_json::Value>(contents)
        .context("decode Storyline CURRENT JSON")?;
    if value.get("control_version").is_some() {
        let control = serde_json::from_value::<StorylineCurrentControl>(value)
            .context("decode Storyline CURRENT control envelope")?;
        anyhow::ensure!(
            control.control_version == CURRENT_CONTROL_VERSION,
            "unsupported Storyline CURRENT control_version {}; expected {}",
            control.control_version,
            CURRENT_CONTROL_VERSION
        );
        return Ok(control);
    }
    let pointer = serde_json::from_value::<StorylineSnapshotPointer>(value)
        .context("decode Storyline snapshot pointer")?;
    Ok(StorylineCurrentControl {
        committed: Some(pointer),
        ..empty_control()
    })
}

pub(super) fn acquire_transition(
    current: &StorylineCurrentControl,
    owner_id: &str,
    now_unix_ms: u64,
    ttl_ms: u64,
) -> Result<(LeaseAcquireOutcome, Option<StorylineCurrentControl>)> {
    anyhow::ensure!(
        !owner_id.trim().is_empty(),
        "writer lease owner must not be empty"
    );
    anyhow::ensure!(ttl_ms > 0, "writer lease TTL must be positive");
    if let Some(lease) = &current.lease
        && lease.expires_at_unix_ms > now_unix_ms
    {
        return Ok((LeaseAcquireOutcome::Held(lease.clone()), None));
    }
    let revision = current
        .revision
        .checked_add(1)
        .context("Storyline CURRENT revision overflow")?;
    let lease = StorylineWriterLease {
        epoch: revision,
        owner_id: owner_id.to_string(),
        issued_at_unix_ms: now_unix_ms,
        expires_at_unix_ms: now_unix_ms.saturating_add(ttl_ms),
        base_generation: current
            .committed
            .as_ref()
            .map(|pointer| pointer.generation.clone()),
    };
    let acquired = AcquiredLease {
        lease: lease.clone(),
        takeover: current.lease.is_some(),
    };
    let mut next = current.clone();
    next.revision = revision;
    next.lease = Some(lease);
    Ok((LeaseAcquireOutcome::Acquired(acquired), Some(next)))
}

fn owns_lease(current: &StorylineCurrentControl, owner_id: &str, epoch: u64) -> bool {
    current.lease.as_ref().is_some_and(|lease| {
        lease.owner_id == owner_id
            && lease.epoch == epoch
            && lease.base_generation
                == current
                    .committed
                    .as_ref()
                    .map(|pointer| pointer.generation.clone())
    })
}

fn publish_transition_with_lease(
    current: &StorylineCurrentControl,
    owner_id: &str,
    epoch: u64,
    now_unix_ms: u64,
    snapshot: &StorylineSnapshotPointer,
    retain_lease: bool,
) -> Result<Option<StorylineCurrentControl>> {
    if !owns_lease(current, owner_id, epoch)
        || current
            .lease
            .as_ref()
            .is_none_or(|lease| lease.expires_at_unix_ms <= now_unix_ms)
    {
        return Ok(None);
    }
    let mut next = current.clone();
    next.revision = next
        .revision
        .checked_add(1)
        .context("Storyline CURRENT revision overflow")?;
    next.committed = Some(snapshot.clone());
    if retain_lease {
        if let Some(lease) = next.lease.as_mut() {
            lease.base_generation = Some(snapshot.generation.clone());
        }
    } else {
        next.lease = None;
    }
    Ok(Some(next))
}

pub(super) fn publish_transition(
    current: &StorylineCurrentControl,
    owner_id: &str,
    epoch: u64,
    now_unix_ms: u64,
    snapshot: &StorylineSnapshotPointer,
) -> Result<Option<StorylineCurrentControl>> {
    publish_transition_with_lease(current, owner_id, epoch, now_unix_ms, snapshot, false)
}

pub(super) fn publish_and_retain_lease_transition(
    current: &StorylineCurrentControl,
    owner_id: &str,
    epoch: u64,
    now_unix_ms: u64,
    snapshot: &StorylineSnapshotPointer,
) -> Result<Option<StorylineCurrentControl>> {
    publish_transition_with_lease(current, owner_id, epoch, now_unix_ms, snapshot, true)
}

pub(super) fn renew_transition(
    current: &StorylineCurrentControl,
    owner_id: &str,
    epoch: u64,
    now_unix_ms: u64,
    ttl_ms: u64,
) -> Result<Option<StorylineCurrentControl>> {
    anyhow::ensure!(ttl_ms > 0, "writer lease TTL must be positive");
    if !owns_lease(current, owner_id, epoch)
        || current
            .lease
            .as_ref()
            .is_none_or(|lease| lease.expires_at_unix_ms <= now_unix_ms)
    {
        return Ok(None);
    }
    let mut next = current.clone();
    next.revision = next
        .revision
        .checked_add(1)
        .context("Storyline CURRENT revision overflow")?;
    if let Some(lease) = next.lease.as_mut() {
        lease.expires_at_unix_ms = now_unix_ms.saturating_add(ttl_ms);
    }
    Ok(Some(next))
}

pub(super) fn release_transition(
    current: &StorylineCurrentControl,
    owner_id: &str,
    epoch: u64,
) -> Result<Option<StorylineCurrentControl>> {
    if !owns_lease(current, owner_id, epoch) {
        return Ok(None);
    }
    let mut next = current.clone();
    next.revision = next
        .revision
        .checked_add(1)
        .context("Storyline CURRENT revision overflow")?;
    next.lease = None;
    Ok(Some(next))
}

pub(super) fn unleased_publish_transition(
    current: &StorylineCurrentControl,
    expected_generation: Option<&str>,
    snapshot: &StorylineSnapshotPointer,
) -> Result<Option<StorylineCurrentControl>> {
    if current.lease.is_some()
        || current
            .committed
            .as_ref()
            .map(|pointer| pointer.generation.as_str())
            != expected_generation
    {
        return Ok(None);
    }
    let mut next = current.clone();
    next.revision = next
        .revision
        .checked_add(1)
        .context("Storyline CURRENT revision overflow")?;
    next.committed = Some(snapshot.clone());
    Ok(Some(next))
}

impl StorylineLanceStore {
    pub(super) async fn read_current_control(&self) -> Result<CurrentControlState> {
        let result = if !self.root_uri.contains("://") {
            match tokio::fs::read(self.root.join(CURRENT_FILE)).await {
                Ok(contents) => Some((contents, None)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            self.control_store
                .read(CURRENT_FILE)
                .await?
                .map(|(contents, version)| (contents, Some(version)))
        };
        let Some((contents, version)) = result else {
            return Ok(CurrentControlState {
                control: empty_control(),
                version: None,
            });
        };
        let contents = std::str::from_utf8(&contents)
            .context("Storyline commit pointer is not valid UTF-8")?
            .trim();
        if !contents.starts_with('{') {
            super::validate_generation_name(contents)?;
            anyhow::bail!(
                "Storyline generation '{contents}' is incomplete: CURRENT must pin all table and object versions"
            );
        }
        let control = decode_control(contents)?;
        validate_current_control(&control)?;
        Ok(CurrentControlState { control, version })
    }

    async fn try_write_current_control(
        &self,
        control: &StorylineCurrentControl,
        expected: Option<Version>,
    ) -> Result<bool> {
        validate_current_control(control)?;
        let contents = serde_json::to_vec(control).context("encode Storyline CURRENT control")?;
        if matches!(self.storage_scheme(), "file" | "file+uring") {
            write_local_current(self.root.join(CURRENT_FILE), contents).await?;
            return Ok(true);
        }
        let result = match expected.as_ref() {
            None => {
                self.control_store
                    .write_create(CURRENT_FILE, contents)
                    .await
            }
            Some(version) => {
                self.control_store
                    .write_match(CURRENT_FILE, contents, version)
                    .await
            }
        };
        match result {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .downcast_ref::<opendal::Error>()
                    .is_some_and(opendal_store::is_conflict) =>
            {
                Ok(false)
            }
            Err(error) => Err(error)
                .with_context(|| format!("update Storyline CURRENT control for {}", self.root_uri)),
        }
    }

    pub(super) async fn try_acquire_writer_lease(
        &self,
        owner_id: &str,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome> {
        let _control_guard = self.control_lock.lock().await;
        for _ in 0..CONTROL_CAS_RETRIES {
            let current = self.read_current_control().await?;
            let (outcome, next) =
                acquire_transition(&current.control, owner_id, now_unix_ms, ttl_ms)?;
            let Some(next) = next else {
                return Ok(outcome);
            };
            if self
                .try_write_current_control(&next, current.version)
                .await?
            {
                return Ok(outcome);
            }
        }
        anyhow::bail!("Storyline commit conflict while acquiring writer lease")
    }

    pub(super) async fn acquire_writer_lease_for_generation(
        &self,
        owner_id: &str,
        expected_generation: Option<&str>,
    ) -> Result<AcquiredLease> {
        let acquired = match self
            .try_acquire_writer_lease(owner_id, unix_now_ms(), WRITER_LEASE_TTL_MS)
            .await?
        {
            LeaseAcquireOutcome::Held(_) => {
                anyhow::bail!("Storyline commit conflict while acquiring writer lease")
            }
            LeaseAcquireOutcome::Acquired(acquired) => acquired,
        };
        if acquired.lease.base_generation.as_deref() == expected_generation {
            return Ok(acquired);
        }
        let conflict = anyhow::anyhow!("Storyline commit conflict while acquiring writer lease");
        match self
            .release_writer_lease(owner_id, acquired.lease.epoch)
            .await
        {
            Ok(true) => Err(conflict),
            Ok(false) => Err(conflict.context("mismatched writer lease was lost before release")),
            Err(error) => Err(conflict.context(format!(
                "failed to release mismatched writer lease: {error:#}"
            ))),
        }
    }

    async fn transition_current_control(
        &self,
        transition: impl Fn(&StorylineCurrentControl) -> Result<Option<StorylineCurrentControl>>,
    ) -> Result<bool> {
        let _control_guard = self.control_lock.lock().await;
        for _ in 0..CONTROL_CAS_RETRIES {
            let current = self.read_current_control().await?;
            let Some(next) = transition(&current.control)? else {
                return Ok(false);
            };
            if self
                .try_write_current_control(&next, current.version)
                .await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) async fn publish_writer_snapshot(
        &self,
        owner_id: &str,
        epoch: u64,
        snapshot: &StorylineSnapshotPointer,
    ) -> Result<bool> {
        self.transition_current_control(|current| {
            publish_transition(current, owner_id, epoch, unix_now_ms(), snapshot)
        })
        .await
    }

    pub(super) async fn publish_writer_snapshot_retaining_lease(
        &self,
        owner_id: &str,
        epoch: u64,
        snapshot: &StorylineSnapshotPointer,
    ) -> Result<bool> {
        self.transition_current_control(|current| {
            publish_and_retain_lease_transition(current, owner_id, epoch, unix_now_ms(), snapshot)
        })
        .await
    }

    pub(super) async fn renew_writer_lease(
        &self,
        owner_id: &str,
        epoch: u64,
        now_unix_ms: u64,
        ttl_ms: u64,
    ) -> Result<bool> {
        self.transition_current_control(|current| {
            renew_transition(current, owner_id, epoch, now_unix_ms, ttl_ms)
        })
        .await
    }

    pub(super) fn start_writer_lease_renewal(
        &self,
        owner_id: String,
        epoch: u64,
    ) -> WriterLeaseRenewal {
        let store = self.clone();
        let lost = Arc::new(AtomicBool::new(false));
        let task_lost = lost.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let interval = std::time::Duration::from_millis(WRITER_LEASE_TTL_MS / 3);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {
                        match store
                            .renew_writer_lease(
                                &owner_id,
                                epoch,
                                unix_now_ms(),
                                WRITER_LEASE_TTL_MS,
                            )
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) | Err(_) => {
                                task_lost.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                    _ = &mut stopped => break,
                }
            }
        });
        WriterLeaseRenewal {
            lost,
            stop: Some(stop),
            task: Some(task),
        }
    }

    pub(super) async fn release_writer_lease(&self, owner_id: &str, epoch: u64) -> Result<bool> {
        self.transition_current_control(|current| release_transition(current, owner_id, epoch))
            .await
    }

    pub(super) async fn try_publish_unleased_snapshot(
        &self,
        snapshot: &StorylineSnapshotPointer,
        expected_generation: Option<&str>,
    ) -> Result<bool> {
        self.transition_current_control(|current| {
            unleased_publish_transition(current, expected_generation, snapshot)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(generation: &str) -> StorylineSnapshotPointer {
        StorylineSnapshotPointer {
            schema_version: crate::store::storyline::STORYLINE_LANCE_SCHEMA_VERSION,
            generation: generation.into(),
            parent_generation: None,
            table_generation: generation.into(),
            runs_version: 1,
            steps_version: 1,
            tool_calls_version: 1,
            objects_version: 1,
            projection: None,
        }
    }

    fn leased_control() -> StorylineCurrentControl {
        StorylineCurrentControl {
            control_version: CURRENT_CONTROL_VERSION,
            revision: 8,
            committed: Some(pointer("gen-1-1-1")),
            lease: Some(StorylineWriterLease {
                epoch: 6,
                owner_id: "writer".into(),
                issued_at_unix_ms: 100,
                expires_at_unix_ms: 300,
                base_generation: Some("gen-1-1-1".into()),
            }),
        }
    }

    #[test]
    fn legacy_pointer_decodes_as_committed_control() {
        let control =
            decode_control(&serde_json::to_string(&pointer("gen-1-1-1")).unwrap()).unwrap();
        assert_eq!(control.committed, Some(pointer("gen-1-1-1")));
        assert!(control.lease.is_none());
    }

    #[test]
    fn live_foreign_lease_is_held_without_mutation() {
        let current = leased_control();
        let (outcome, next) = acquire_transition(&current, "right", 150, 100).unwrap();
        assert!(matches!(outcome, LeaseAcquireOutcome::Held(_)));
        assert!(next.is_none());
    }

    #[test]
    fn expired_lease_takeover_advances_epoch() {
        let current = leased_control();
        let (outcome, next) = acquire_transition(&current, "right", 300, 100).unwrap();
        let LeaseAcquireOutcome::Acquired(acquired) = outcome else {
            panic!("expired lease must be acquired");
        };
        assert!(acquired.takeover);
        assert_eq!(acquired.lease.epoch, 9);
        assert_eq!(next.unwrap().revision, 9);
    }

    #[test]
    fn stale_or_expired_owner_cannot_publish_or_release() {
        let current = leased_control();
        assert!(
            publish_transition(&current, "old", 5, 200, &pointer("gen-2-1-1"))
                .unwrap()
                .is_none()
        );
        assert!(
            publish_transition(&current, "writer", 6, 300, &pointer("gen-2-1-1"))
                .unwrap()
                .is_none()
        );
        assert!(release_transition(&current, "old", 5).unwrap().is_none());
    }

    #[test]
    fn renewal_and_retained_publication_preserve_ownership() {
        let current = leased_control();
        let renewed = renew_transition(&current, "writer", 6, 200, 100)
            .unwrap()
            .unwrap();
        assert_eq!(renewed.lease.as_ref().unwrap().expires_at_unix_ms, 300);
        let mut snapshot = pointer("gen-2-1-1");
        snapshot.parent_generation = Some("gen-1-1-1".into());
        let published = publish_and_retain_lease_transition(&renewed, "writer", 6, 250, &snapshot)
            .unwrap()
            .unwrap();
        assert_eq!(
            published.lease.unwrap().base_generation.as_deref(),
            Some("gen-2-1-1")
        );
    }
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    fn pointer(generation: &str) -> StorylineSnapshotPointer {
        StorylineSnapshotPointer {
            schema_version: crate::store::storyline::STORYLINE_LANCE_SCHEMA_VERSION,
            generation: generation.into(),
            parent_generation: None,
            table_generation: generation.into(),
            runs_version: 1,
            steps_version: 1,
            tool_calls_version: 1,
            objects_version: 1,
            projection: None,
        }
    }

    proptest! {
        #[test]
        fn fresh_acquisition_is_held_until_expiry(
            owner in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
            now in 0u64..1_000_000,
            ttl in 1u64..10_000,
        ) {
            let current = empty_control();
            let (outcome, next) = acquire_transition(&current, &owner, now, ttl).unwrap();
            let LeaseAcquireOutcome::Acquired(acquired) = outcome else {
                panic!("empty control must acquire");
            };
            let next = next.expect("acquisition must produce next control");
            prop_assert_eq!(acquired.lease.epoch, 1);
            prop_assert_eq!(acquired.lease.expires_at_unix_ms, now + ttl);
            let (held, unchanged) = acquire_transition(&next, "other", now, ttl).unwrap();
            prop_assert!(matches!(held, LeaseAcquireOutcome::Held(_)));
            prop_assert!(unchanged.is_none());
        }

        #[test]
        fn valid_publish_and_renewal_advance_revision_and_preserve_identity(
            now in 0u64..1_000_000,
            ttl in 1u64..10_000,
            generation in proptest::string::string_regex("gen-[A-Za-z0-9_-]{1,16}").unwrap(),
        ) {
            let (outcome, current) = acquire_transition(&empty_control(), "writer", now, ttl).unwrap();
            let acquired = match outcome { LeaseAcquireOutcome::Acquired(value) => value, _ => unreachable!() };
            let current = current.unwrap();
            let renewed = renew_transition(&current, "writer", acquired.lease.epoch, now, ttl).unwrap().unwrap();
            prop_assert_eq!(renewed.revision, current.revision + 1);
            prop_assert_eq!(&renewed.lease.as_ref().unwrap().owner_id, "writer");
            let published = publish_transition(&renewed, "writer", acquired.lease.epoch, now, &pointer(&generation)).unwrap().unwrap();
            prop_assert_eq!(published.revision, renewed.revision + 1);
            prop_assert_eq!(&published.committed.as_ref().unwrap().generation, &generation);
            prop_assert!(published.lease.is_none());
        }

        #[test]
        fn expired_leases_can_only_be_taken_over_with_a_new_epoch(
            now in 1u64..1_000_000,
            ttl in 1u64..10_000,
            revision in 0u64..1_000_000,
            old_owner in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
            new_owner in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        ) {
            prop_assume!(old_owner != new_owner);
            let current = StorylineCurrentControl {
                control_version: CURRENT_CONTROL_VERSION,
                revision,
                committed: None,
                lease: Some(StorylineWriterLease {
                    epoch: revision.saturating_add(1),
                    owner_id: old_owner,
                    issued_at_unix_ms: now.saturating_sub(ttl),
                    expires_at_unix_ms: now,
                    base_generation: None,
                }),
            };
            let (outcome, next) = acquire_transition(&current, &new_owner, now, ttl).unwrap();
            let LeaseAcquireOutcome::Acquired(acquired) = outcome else {
                panic!("expired lease must be taken over");
            };
            prop_assert!(acquired.takeover);
            prop_assert_eq!(acquired.lease.epoch, revision.saturating_add(1));
            prop_assert_eq!(next.unwrap().lease.unwrap().owner_id, new_owner);
        }
    }
}
