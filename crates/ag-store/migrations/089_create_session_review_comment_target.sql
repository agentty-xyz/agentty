CREATE TABLE session_review_comment_target (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    review_request_display_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    PRIMARY KEY (session_id, review_request_display_id, thread_id)
);
