#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    zstandard::fuzz::sequence_parse(data);
});
