#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = persisting_pvisor::decode_agent_abi_frame_for_fuzz(data);
});
