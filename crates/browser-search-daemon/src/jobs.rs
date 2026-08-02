use std::{
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use dashmap::DashMap;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use uuid::Uuid;

use crate::{
    bridge::BridgeHub,
    config::Config,
    error::{ErrorCode, ErrorDetail},
    model::{SearchCommand, SearchResult},
};

pub type SearchOutcome = Result<Vec<SearchResult>, ErrorDetail>;
type PendingResponse = (oneshot::Sender<SearchOutcome>, SearchOutcome);

#[derive(Clone, Copy, PartialEq, Eq)]
enum DispatchPhase {
    Queued,
    Dispatching,
    Dispatched,
}

struct PendingState {
    sender: Option<oneshot::Sender<SearchOutcome>>,
    permit: Option<OwnedSemaphorePermit>,
    phase: DispatchPhase,
    /// The HTTP caller has received its terminal outcome.
    responded: bool,
    /// Cancellation happened while the Bridge search message was being sent.
    cancel_requested: bool,
    pending_cancel_reason: Option<String>,
    /// The extension has received a cancellation and still owes cleanup confirmation.
    awaiting_cleanup: bool,
    /// This job contributes one entry to the scheduler cleanup gate.
    cleanup_blocker_registered: bool,
    /// Both the caller response and browser-resource lifecycle are complete.
    finalized: bool,
}

struct PendingJob {
    id: Uuid,
    command: SearchCommand,
    state: Mutex<PendingState>,
}

enum DispatchCompletion {
    Sent,
    SendCancel(Option<String>),
    Finished {
        response: Option<PendingResponse>,
        permit: Option<OwnedSemaphorePermit>,
        had_cleanup_blocker: bool,
    },
    Skipped,
}

enum CancellationResult {
    AlreadyHandled,
    Finished {
        response: Option<PendingResponse>,
        permit: Option<OwnedSemaphorePermit>,
        had_cleanup_blocker: bool,
    },
    PendingCleanup {
        response: Option<PendingResponse>,
        send_cancel: Option<String>,
        register_blocker: bool,
    },
}

struct CleanupResult {
    response: Option<PendingResponse>,
    permit: Option<OwnedSemaphorePermit>,
    had_cleanup_blocker: bool,
}

impl PendingJob {
    fn new(command: SearchCommand, sender: oneshot::Sender<SearchOutcome>) -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::new_v4(),
            command,
            state: Mutex::new(PendingState {
                sender: Some(sender),
                permit: None,
                phase: DispatchPhase::Queued,
                responded: false,
                cancel_requested: false,
                pending_cancel_reason: None,
                awaiting_cleanup: false,
                cleanup_blocker_registered: false,
                finalized: false,
            }),
        })
    }

    async fn prepare_dispatch(&self, permit: OwnedSemaphorePermit) -> bool {
        let mut state = self.state.lock().await;
        if state.responded || state.finalized || state.phase != DispatchPhase::Queued {
            return false;
        }
        state.permit = Some(permit);
        state.phase = DispatchPhase::Dispatching;
        true
    }

    async fn finish_dispatch(&self, result: Result<(), ErrorDetail>) -> DispatchCompletion {
        let mut state = self.state.lock().await;
        if state.finalized {
            return DispatchCompletion::Skipped;
        }

        match result {
            Ok(()) => {
                state.phase = DispatchPhase::Dispatched;
                if state.cancel_requested {
                    state.awaiting_cleanup = true;
                    DispatchCompletion::SendCancel(state.pending_cancel_reason.take())
                } else {
                    DispatchCompletion::Sent
                }
            }
            Err(error) => {
                let response = if state.responded {
                    None
                } else {
                    state.responded = true;
                    state.sender.take().map(|sender| (sender, Err(error)))
                };
                state.finalized = true;
                DispatchCompletion::Finished {
                    response,
                    permit: state.permit.take(),
                    had_cleanup_blocker: state.cleanup_blocker_registered,
                }
            }
        }
    }

    async fn request_cancellation(
        &self,
        outcome: SearchOutcome,
        cancel_reason: Option<&str>,
    ) -> CancellationResult {
        let mut state = self.state.lock().await;
        if state.finalized {
            return CancellationResult::AlreadyHandled;
        }
        if state.responded {
            return if state.cancel_requested || state.awaiting_cleanup {
                CancellationResult::PendingCleanup {
                    response: None,
                    send_cancel: None,
                    register_blocker: false,
                }
            } else {
                CancellationResult::AlreadyHandled
            };
        }

        state.responded = true;
        let response = state.sender.take().map(|sender| (sender, outcome));
        match state.phase {
            DispatchPhase::Queued => {
                state.finalized = true;
                CancellationResult::Finished {
                    response,
                    permit: state.permit.take(),
                    had_cleanup_blocker: false,
                }
            }
            DispatchPhase::Dispatching => {
                state.cancel_requested = true;
                state.pending_cancel_reason = cancel_reason.map(str::to_owned);
                state.cleanup_blocker_registered = true;
                CancellationResult::PendingCleanup {
                    response,
                    send_cancel: None,
                    register_blocker: true,
                }
            }
            DispatchPhase::Dispatched => {
                state.awaiting_cleanup = true;
                state.cleanup_blocker_registered = true;
                CancellationResult::PendingCleanup {
                    response,
                    send_cancel: cancel_reason.map(str::to_owned),
                    register_blocker: true,
                }
            }
        }
    }

    async fn finish_after_cleanup(
        &self,
        outcome: Option<SearchOutcome>,
        require_cleanup_wait: bool,
    ) -> Option<CleanupResult> {
        let mut state = self.state.lock().await;
        if state.finalized || state.phase == DispatchPhase::Queued {
            return None;
        }
        if require_cleanup_wait && !(state.cancel_requested || state.awaiting_cleanup) {
            return None;
        }

        let response = if state.responded {
            None
        } else {
            let outcome = outcome?;
            state.responded = true;
            state.sender.take().map(|sender| (sender, outcome))
        };
        state.cancel_requested = false;
        state.pending_cancel_reason = None;
        state.awaiting_cleanup = false;
        let had_cleanup_blocker = state.cleanup_blocker_registered;
        state.cleanup_blocker_registered = false;
        state.finalized = true;
        Some(CleanupResult {
            response,
            permit: state.permit.take(),
            had_cleanup_blocker,
        })
    }
}

pub struct JobScheduler {
    config: Arc<Config>,
    bridge: Arc<BridgeHub>,
    jobs: DashMap<Uuid, Arc<PendingJob>>,
    queue_tx: mpsc::Sender<Uuid>,
    queue_rx: StdMutex<Option<mpsc::Receiver<Uuid>>>,
    browser_slots: Arc<Semaphore>,
    cleanup_gate: Mutex<usize>,
    cleanup_gate_notify: Notify,
    cleanup_owner: Mutex<Option<String>>,
    queued_jobs: AtomicUsize,
    active_jobs: AtomicUsize,
}

impl JobScheduler {
    pub fn new(config: Arc<Config>, bridge: Arc<BridgeHub>) -> Self {
        let (queue_tx, queue_rx) = mpsc::channel(config.executor.max_queue_size);
        Self {
            browser_slots: Arc::new(Semaphore::new(config.executor.max_concurrency)),
            config,
            bridge,
            jobs: DashMap::new(),
            queue_tx,
            queue_rx: StdMutex::new(Some(queue_rx)),
            cleanup_gate: Mutex::new(0),
            cleanup_gate_notify: Notify::new(),
            cleanup_owner: Mutex::new(None),
            queued_jobs: AtomicUsize::new(0),
            active_jobs: AtomicUsize::new(0),
        }
    }

    pub fn start(self: Arc<Self>) {
        let receiver = self
            .queue_rx
            .lock()
            .expect("scheduler queue mutex poisoned")
            .take()
            .expect("scheduler can only be started once");
        tokio::spawn(self.clone().dispatch_loop(receiver));
    }

    pub async fn enqueue(
        self: &Arc<Self>,
        command: SearchCommand,
    ) -> Result<(Uuid, oneshot::Receiver<SearchOutcome>), ErrorDetail> {
        if !self.bridge.is_connected().await {
            return Err(ErrorDetail::browser_unavailable("Chrome 扩展尚未连接"));
        }

        let timeout_ms = command.timeout_ms;
        let (sender, receiver) = oneshot::channel();
        let job = PendingJob::new(command, sender);
        let id = job.id;
        self.jobs.insert(id, job);
        self.queued_jobs.fetch_add(1, Ordering::Relaxed);
        if self.queue_tx.try_send(id).is_err() {
            self.queued_jobs.fetch_sub(1, Ordering::Relaxed);
            self.jobs.remove(&id);
            return Err(ErrorDetail::new(
                ErrorCode::QueueFull,
                "搜索任务队列已满",
                true,
            ));
        }

        let scheduler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
            scheduler.timeout_job(id).await;
        });
        Ok((id, receiver))
    }

    async fn dispatch_loop(self: Arc<Self>, mut receiver: mpsc::Receiver<Uuid>) {
        while let Some(id) = receiver.recv().await {
            self.queued_jobs.fetch_sub(1, Ordering::Relaxed);
            let Some(job) = self.jobs.get(&id).map(|entry| entry.clone()) else {
                continue;
            };

            let permit = match self.browser_slots.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => break,
            };
            self.active_jobs.fetch_add(1, Ordering::Relaxed);
            let cleanup_guard = self.cleanup_guard().await;
            if !job.prepare_dispatch(permit).await {
                drop(cleanup_guard);
                self.active_jobs.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            drop(cleanup_guard);

            let send_result = self.bridge.send_search(id, &job.command).await;
            match job.finish_dispatch(send_result).await {
                DispatchCompletion::Sent | DispatchCompletion::Skipped => {}
                DispatchCompletion::SendCancel(reason) => {
                    if let Some(reason) = reason {
                        let _ = self.bridge.send_cancel(id, &reason).await;
                    }
                }
                DispatchCompletion::Finished {
                    response,
                    permit,
                    had_cleanup_blocker,
                } => {
                    self.jobs.remove(&id);
                    self.release_cleanup_blocker(had_cleanup_blocker).await;
                    self.release_active_permit(permit);
                    Self::send_response(response);
                }
            }
        }
    }

    pub async fn complete(&self, id: Uuid, mut results: Vec<SearchResult>) {
        let Some(job) = self.jobs.get(&id).map(|entry| entry.clone()) else {
            return;
        };
        results.retain(|result| !result.is_empty());
        results.truncate(job.command.limit as usize);
        self.finish_with_cleanup(id, Some(Ok(results)), false).await;
    }

    pub async fn extension_error(&self, id: Uuid, error: ErrorDetail) {
        self.finish_with_cleanup(id, Some(Err(error)), false).await;
    }

    pub async fn cleanup_complete(&self, id: Uuid) {
        self.finish_with_cleanup(id, None, true).await;
    }

    pub async fn protocol_error(&self, id: Uuid, error: ErrorDetail) {
        self.cancel_job(id, Err(error), None).await;
    }

    pub async fn cancel(&self, id: Uuid, reason: &str) {
        self.cancel_job(
            id,
            Err(ErrorDetail::new(
                ErrorCode::Cancelled,
                reason.to_owned(),
                false,
            )),
            Some(reason),
        )
        .await;
    }

    async fn timeout_job(&self, id: Uuid) {
        self.cancel_job(
            id,
            Err(ErrorDetail::timeout("搜索任务超时")),
            Some("timeout"),
        )
        .await;
    }

    async fn cancel_job(
        &self,
        id: Uuid,
        outcome: SearchOutcome,
        cancel_reason: Option<&str>,
    ) -> bool {
        let Some(job) = self.jobs.get(&id).map(|entry| entry.clone()) else {
            return false;
        };
        match self
            .cancellation_transition(&job, outcome, cancel_reason)
            .await
        {
            CancellationResult::AlreadyHandled => false,
            CancellationResult::Finished {
                response,
                permit,
                had_cleanup_blocker,
            } => {
                self.jobs.remove(&id);
                self.release_cleanup_blocker(had_cleanup_blocker).await;
                self.release_active_permit(permit);
                Self::send_response(response);
                false
            }
            CancellationResult::PendingCleanup { response, .. } => {
                Self::send_response(response);
                true
            }
        }
    }

    async fn cancellation_transition(
        &self,
        job: &PendingJob,
        outcome: SearchOutcome,
        cancel_reason: Option<&str>,
    ) -> CancellationResult {
        let mut cleanup_gate = self.cleanup_gate.lock().await;
        let result = job.request_cancellation(outcome, cancel_reason).await;
        if let CancellationResult::PendingCleanup {
            register_blocker, ..
        } = &result
            && *register_blocker
        {
            *cleanup_gate += 1;
        }
        let send_cancel = match &result {
            CancellationResult::PendingCleanup { send_cancel, .. } => send_cancel.clone(),
            _ => None,
        };
        drop(cleanup_gate);
        if let Some(reason) = send_cancel {
            let _ = self.bridge.send_cancel(job.id, &reason).await;
        }
        result
    }

    async fn finish_with_cleanup(
        &self,
        id: Uuid,
        outcome: Option<SearchOutcome>,
        require_cleanup_wait: bool,
    ) {
        let Some(job) = self.jobs.get(&id).map(|entry| entry.clone()) else {
            return;
        };
        let Some(result) = job
            .finish_after_cleanup(outcome, require_cleanup_wait)
            .await
        else {
            return;
        };
        self.jobs.remove(&id);
        self.release_cleanup_blocker(result.had_cleanup_blocker)
            .await;
        self.release_active_permit(result.permit);
        Self::send_response(result.response);
    }

    fn send_response(response: Option<PendingResponse>) {
        if let Some((sender, outcome)) = response {
            let _ = sender.send(outcome);
        }
    }

    fn release_active_permit(&self, permit: Option<OwnedSemaphorePermit>) {
        if let Some(permit) = permit {
            self.active_jobs.fetch_sub(1, Ordering::Relaxed);
            drop(permit);
        }
    }

    async fn cleanup_guard(&self) -> tokio::sync::MutexGuard<'_, usize> {
        loop {
            let notified = self.cleanup_gate_notify.notified();
            let gate = self.cleanup_gate.lock().await;
            if *gate == 0 {
                return gate;
            }
            drop(gate);
            notified.await;
        }
    }

    async fn release_cleanup_blocker(&self, had_cleanup_blocker: bool) {
        if !had_cleanup_blocker {
            return;
        }
        let mut gate = self.cleanup_gate.lock().await;
        debug_assert!(*gate > 0);
        *gate -= 1;
        let should_notify = *gate == 0;
        drop(gate);
        if should_notify {
            self.cleanup_gate_notify.notify_one();
        }
    }

    pub async fn required_cleanup_instance(&self) -> Option<String> {
        self.cleanup_owner.lock().await.clone()
    }

    pub async fn handle_disconnect(&self, browser_instance_id: &str) {
        let ids = self
            .jobs
            .iter()
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        let mut cleanup_pending = false;
        for id in ids {
            cleanup_pending |= self
                .cancel_job(
                    id,
                    Err(ErrorDetail::browser_unavailable("Chrome 扩展连接已断开")),
                    None,
                )
                .await;
        }
        let mut owner = self.cleanup_owner.lock().await;
        *owner = cleanup_pending.then(|| browser_instance_id.to_owned());
    }

    pub async fn handle_reconnect(&self, browser_instance_id: &str) {
        let owner_matches = self
            .cleanup_owner
            .lock()
            .await
            .as_deref()
            .is_some_and(|owner| owner == browser_instance_id);
        if !owner_matches {
            return;
        }

        let ids = self
            .jobs
            .iter()
            .map(|entry| *entry.key())
            .collect::<Vec<_>>();
        for id in ids {
            self.finish_with_cleanup(id, None, true).await;
        }
        self.cleanup_owner.lock().await.take();
    }

    pub fn active_jobs(&self) -> usize {
        self.active_jobs.load(Ordering::Relaxed)
    }

    pub fn queued_jobs(&self) -> usize {
        self.queued_jobs.load(Ordering::Relaxed)
    }

    pub fn max_concurrency(&self) -> usize {
        self.config.executor.max_concurrency
    }
}
