#![no_main]
use libfuzzer_sys::fuzz_target;
//use h2::hpack;

fuzz_target!(|data_: &[u8]| {
    // fuzzed code goes here
    //let decoder_ = h2::fuzz_bridge::fuzz_logic::fuzz_addr_1("sdfgsdg");
    let decoder_ = h2::fuzz_bridge::fuzz_logic::fuzz_addr_1(data_);
});
