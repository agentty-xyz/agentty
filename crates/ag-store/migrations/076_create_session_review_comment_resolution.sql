CREATE TABLE session_review_comment_resolution (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    commit_hash TEXT,
    review_request_display_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    reply TEXT NOT NULL,
    reply_token TEXT NOT NULL UNIQUE,
    resolution TEXT NOT NULL CHECK (resolution IN ('fixed', 'no_change_needed')),
    is_posting INTEGER NOT NULL DEFAULT 0 CHECK (is_posting IN (0, 1)),
    PRIMARY KEY (session_id, review_request_display_id, thread_id)
);
