#![no_main]

use ag_protocol::parse_agent_response_strict;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let raw_response = String::from_utf8_lossy(data);

    let _ = parse_agent_response_strict(&raw_response);
});
