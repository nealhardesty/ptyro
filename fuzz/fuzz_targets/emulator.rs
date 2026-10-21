#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| ptyro_testkit::fuzz::emulator(data));
