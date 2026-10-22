//! Serializes Ethernet trials and retains rollback until apply/save actually succeeds (H15-63).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::Instant;

use super::ethernet::{Ethernet, Sections, TRIAL};

pub(super) trait Backend: Clone + Send + Sync + 'static {
    fn applied_sections(&self) -> impl Future<Output = anyhow::Result<Sections>> + Send;
    fn reapply(&self, next: Sections) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn persist(&self, next: Sections) -> impl Future<Output = anyhow::Result<()>> + Send;
}

impl Backend for Ethernet {
    async fn applied_sections(&self) -> anyhow::Result<Sections> {
        Ethernet::applied_sections(self).await
    }
    async fn reapply(&self, next: Sections) -> anyhow::Result<()> {
        Ethernet::reapply(self, next).await
    }
    async fn persist(&self, next: Sections) -> anyhow::Result<()> {
        Ethernet::persist(self, next).await
    }
}

#[derive(Debug)]
struct Pending {
    id: u64,
    previous: Sections,
    next: Sections,
    deadline: Instant,
}

#[derive(Debug, Default)]
pub(super) struct Trials {
    pending: Mutex<Option<Pending>>,
    operation: tokio::sync::Mutex<()>,
}

impl Trials {
    pub fn left(&self) -> u32 {
        self.pending.lock().expect("trial lock").as_ref().map_or(0, |t| {
            u32::try_from(t.deadline.saturating_duration_since(Instant::now()).as_secs())
                .unwrap_or(u32::MAX)
                .max(1)
        })
    }

    pub async fn apply(
        self: &Arc<Self>,
        backend: impl Backend,
        requested: Sections,
        changed: Arc<Notify>,
    ) -> anyhow::Result<u32> {
        let _operation = self.operation.lock().await;
        let current = backend.applied_sections().await?;
        let (mut previous, mut next) = match self.pending.lock().expect("trial lock").as_ref() {
            Some(t) => (copy_sections(&t.previous)?, copy_sections(&t.next)?),
            None => (Sections::new(), Sections::new()),
        };
        // Capture only touched families; an IPv6-only trial never rewrites IPv4 on rollback.
        let mut restore_attempt = Sections::new();
        for name in requested.keys() {
            let before = current
                .get(name)
                .map(copy_section)
                .transpose()?
                .unwrap_or_default();
            previous.entry(name.clone()).or_insert(copy_section(&before)?);
            restore_attempt.insert(name.clone(), before);
        }
        next.extend(copy_sections(&requested)?);
        if let Err(error) = backend.reapply(requested).await {
            // Reapply can fail after some work: recover the state before this attempt, while
            // leaving any earlier trial and its timer intact.
            if let Err(restore) = backend.reapply(restore_attempt).await {
                tracing::error!(error = %restore, "restoring a failed Ethernet attempt failed");
            }
            return Err(error);
        }
        let id = super::rand_id();
        let deadline = Instant::now() + TRIAL;
        *self.pending.lock().expect("trial lock") = Some(Pending {
            id,
            previous,
            next,
            deadline,
        });
        self.arm(backend, id, deadline, changed);
        Ok(u32::try_from(TRIAL.as_secs()).unwrap_or(60))
    }

    fn arm(self: &Arc<Self>, backend: impl Backend, id: u64, deadline: Instant, changed: Arc<Notify>) {
        let trials = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            loop {
                let result = trials.revert(&backend, id).await;
                changed.notify_one();
                match result {
                    Ok(_) => break,
                    Err(error) => tracing::error!(%error, "Ethernet rollback failed; retrying in 5 s"),
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    pub async fn confirm(&self, backend: &impl Backend) -> anyhow::Result<()> {
        let _operation = self.operation.lock().await;
        let next = {
            let pending = self.pending.lock().expect("trial lock");
            let trial = pending
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("no Ethernet change is waiting for confirmation"))?;
            anyhow::ensure!(Instant::now() < trial.deadline, "the Ethernet trial has expired");
            copy_sections(&trial.next)?
        };
        backend.persist(next).await?;
        self.pending.lock().expect("trial lock").take();
        Ok(())
    }

    async fn revert(&self, backend: &impl Backend, id: u64) -> anyhow::Result<bool> {
        let _operation = self.operation.lock().await;
        let previous = {
            let pending = self.pending.lock().expect("trial lock");
            let Some(trial) = pending.as_ref().filter(|t| t.id == id) else {
                return Ok(false);
            };
            copy_sections(&trial.previous)?
        };
        backend.reapply(previous).await?;
        self.pending.lock().expect("trial lock").take();
        tracing::warn!("Ethernet change not confirmed; reverted");
        Ok(true)
    }
}

fn copy_sections(sections: &Sections) -> anyhow::Result<Sections> {
    sections
        .iter()
        .map(|(k, v)| Ok((k.clone(), copy_section(v)?)))
        .collect()
}

fn copy_section(section: &super::ethernet::Section) -> anyhow::Result<super::ethernet::Section> {
    section
        .iter()
        .map(|(k, v)| Ok((k.clone(), v.try_clone()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedValue, Value};

    #[derive(Debug, Default, Clone)]
    struct Fake(Arc<Mutex<FakeState>>);

    #[derive(Debug, Default)]
    struct FakeState {
        applied: Sections,
        saved: Sections,
        fail_apply: bool,
        fail_save: bool,
    }

    fn section(method: &str) -> Sections {
        let mut section = std::collections::HashMap::new();
        section.insert(
            "method".into(),
            OwnedValue::try_from(Value::from(method)).unwrap(),
        );
        Sections::from([("ipv6".into(), section)])
    }

    impl Backend for Fake {
        fn applied_sections(&self) -> impl Future<Output = anyhow::Result<Sections>> + Send {
            std::future::ready(copy_sections(&self.0.lock().unwrap().applied))
        }
        async fn reapply(&self, next: Sections) -> anyhow::Result<()> {
            tokio::task::yield_now().await;
            let mut state = self.0.lock().unwrap();
            if std::mem::take(&mut state.fail_apply) {
                state.applied.extend(next);
                anyhow::bail!("simulated partial reapply failure");
            }
            state.applied.extend(next);
            Ok(())
        }
        async fn persist(&self, next: Sections) -> anyhow::Result<()> {
            tokio::task::yield_now().await;
            let mut state = self.0.lock().unwrap();
            if std::mem::take(&mut state.fail_save) {
                anyhow::bail!("simulated save failure");
            }
            state.saved.extend(next);
            Ok(())
        }
    }

    fn setup() -> (Arc<Trials>, Fake, Arc<tokio::sync::Notify>) {
        let fake = Fake::default();
        {
            let mut state = fake.0.lock().unwrap();
            state.applied = section("auto");
            state.saved = section("auto");
        }
        (Arc::default(), fake, Arc::default())
    }

    #[tokio::test]
    async fn failed_confirmation_keeps_the_automatic_rollback() {
        let (trials, fake, changed) = setup();
        trials
            .apply(fake.clone(), section("disabled"), changed)
            .await
            .unwrap();
        let id = trials.pending.lock().unwrap().as_ref().unwrap().id;
        fake.0.lock().unwrap().fail_save = true;
        assert!(trials.confirm(&fake).await.is_err());
        assert!(trials.left() > 0);
        assert!(trials.revert(&fake, id).await.unwrap());
        let state = fake.0.lock().unwrap();
        assert_eq!(state.applied, section("auto"));
        assert_eq!(state.saved, section("auto"));
    }

    #[tokio::test]
    async fn failed_replacement_does_not_cancel_the_original_trial() {
        let (trials, fake, changed) = setup();
        trials
            .apply(fake.clone(), section("disabled"), changed.clone())
            .await
            .unwrap();
        let id = trials.pending.lock().unwrap().as_ref().unwrap().id;
        fake.0.lock().unwrap().fail_apply = true;
        assert!(
            trials
                .apply(fake.clone(), section("dhcp"), changed)
                .await
                .is_err()
        );
        assert_eq!(trials.pending.lock().unwrap().as_ref().unwrap().id, id);
        assert_eq!(fake.0.lock().unwrap().applied, section("disabled"));
        trials.revert(&fake, id).await.unwrap();
        assert_eq!(fake.0.lock().unwrap().applied, section("auto"));
    }

    #[tokio::test]
    async fn old_timer_cannot_revert_a_successful_replacement() {
        let (trials, fake, changed) = setup();
        trials
            .apply(fake.clone(), section("disabled"), changed.clone())
            .await
            .unwrap();
        let old = trials.pending.lock().unwrap().as_ref().unwrap().id;
        trials
            .apply(fake.clone(), section("dhcp"), changed)
            .await
            .unwrap();
        assert!(!trials.revert(&fake, old).await.unwrap());
        trials.confirm(&fake).await.unwrap();
        let state = fake.0.lock().unwrap();
        assert_eq!(state.applied, section("dhcp"));
        assert_eq!(state.saved, section("dhcp"));
    }

    #[tokio::test]
    async fn concurrent_confirmation_and_expiry_leave_applied_and_saved_in_agreement() {
        let (trials, fake, changed) = setup();
        trials
            .apply(fake.clone(), section("disabled"), changed)
            .await
            .unwrap();
        let id = trials.pending.lock().unwrap().as_ref().unwrap().id;
        let _ = tokio::join!(trials.confirm(&fake), trials.revert(&fake, id));
        let state = fake.0.lock().unwrap();
        assert_eq!(state.applied, state.saved);
        assert_eq!(trials.left(), 0);
    }
}
