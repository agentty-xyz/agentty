UPDATE session
SET status = 'Review'
WHERE status IN ('PullRequest', 'CreatingPullRequest', 'Processing');
