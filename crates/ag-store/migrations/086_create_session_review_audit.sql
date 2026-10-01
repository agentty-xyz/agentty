-- Keep the latest completed review's discovery and group-scoped decisions.
CREATE TABLE session_review_audit (
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    generation TEXT NOT NULL,
    request TEXT NOT NULL,
    answer TEXT NOT NULL,
    PRIMARY KEY (session_id, request)
);
