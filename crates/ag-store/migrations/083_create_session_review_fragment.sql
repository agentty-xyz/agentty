-- Successful review calls survive interruption and bounded-budget retries.
CREATE TABLE session_review_fragment (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    generation TEXT NOT NULL,
    request TEXT NOT NULL,
    answer TEXT NOT NULL,
    PRIMARY KEY (session_id, generation, request)
);
