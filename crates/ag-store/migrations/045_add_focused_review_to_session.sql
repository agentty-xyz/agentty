ALTER TABLE session
ADD COLUMN focused_review_text TEXT;

ALTER TABLE session
ADD COLUMN focused_review_diff_hash TEXT;
