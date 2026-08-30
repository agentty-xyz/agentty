use crate::schema_contract;

const JSON_STRING_MAX_EXPANSION: usize = 6;

pub(crate) const ERROR_BODY_LIMIT_BYTES: usize = 4 * 1024;
pub(crate) const RESPONSE_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
pub(crate) const SUCCESS_BODY_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_ENVELOPE_LIMIT_BYTES;
