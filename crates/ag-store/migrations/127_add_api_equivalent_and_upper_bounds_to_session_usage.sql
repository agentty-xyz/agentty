-- Keeps the API equivalent of plan-billed usage apart from the estimated
-- charge of metered usage, so the two are never summed, and records the
-- subscription login that billed it: chatgpt, claude, or mixed for a sum
-- over several logins. Either amount is an upper bound once a priced
-- invocation read cached input from a model with no declared cache price,
-- so those reads were priced at the input price.
ALTER TABLE session_usage ADD COLUMN is_cost_upper_bound INTEGER NOT NULL DEFAULT 0 CHECK (
    is_cost_upper_bound IN (0, 1)
);
-- Sum over the plan-billed invocations; NULL until one is priced.
ALTER TABLE session_usage ADD COLUMN api_equivalent_micro_usd INTEGER CHECK (
    api_equivalent_micro_usd >= 0
);
-- The login is set exactly when the amount is.
ALTER TABLE session_usage ADD COLUMN api_equivalent_login TEXT CHECK (
    (api_equivalent_login IS NULL) = (api_equivalent_micro_usd IS NULL)
    AND (api_equivalent_login IS NULL OR api_equivalent_login IN ('chatgpt', 'claude', 'mixed'))
);
ALTER TABLE session_usage ADD COLUMN is_api_equivalent_upper_bound INTEGER NOT NULL DEFAULT 0 CHECK (
    is_api_equivalent_upper_bound IN (0, 1)
);
