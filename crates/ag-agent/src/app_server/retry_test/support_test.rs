use super::*;

#[derive(Debug)]
pub(super) struct TestRuntime {
    pub(super) model: String,
}

impl TestRuntime {
    pub(super) fn shutdown(&mut self) -> BorrowedAppServerFuture<'_, ()> {
        Box::pin(async move {
            self.model = "stopped".into();
        })
    }
}

#[derive(Debug)]
pub(super) struct TestLiveTranscript {
    pub(super) text: String,
}

impl LiveTranscript for TestLiveTranscript {
    fn replay_text(&self) -> Option<String> {
        Some(self.text.clone())
    }
}

pub(super) fn live_transcript(text: &str) -> Arc<dyn LiveTranscript> {
    Arc::new(TestLiveTranscript {
        text: text.to_string(),
    })
}

pub(super) fn session_start_request_kind() -> AgentRequestKind {
    AgentRequestKind::SessionStart
}

pub(super) fn session_resume_request_kind() -> AgentRequestKind {
    AgentRequestKind::SessionResume
}
