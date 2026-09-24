use async_trait::async_trait;
use tokio::sync::Notify;

pub(super) struct AcquisitionGate {
    pub(super) entered: Notify,
    pub(super) release: Notify,
}

#[async_trait]
impl crate::session::ReservationObserver for AcquisitionGate {
    async fn committed(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

pub(super) struct CommitGate {
    pub(super) entered: Notify,
    pub(super) fail: bool,
    pub(super) release: Notify,
}

#[async_trait]
impl crate::session::ReservationObserver for CommitGate {
    async fn committing(&self) {
        self.entered.notify_one();
        self.release.notified().await;
        assert!(!self.fail, "injected committer task failure");
    }

    async fn committed(&self) {}
}
