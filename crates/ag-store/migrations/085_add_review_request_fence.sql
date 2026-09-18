-- Fence late checkpoint writes across retries and explicit context invalidation.
ALTER TABLE session_review_generation ADD COLUMN request_id TEXT NOT NULL DEFAULT '';
