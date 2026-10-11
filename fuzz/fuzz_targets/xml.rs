#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| fastfind_fuzz::parse(&fastfind_core::parsers::xml::XmlParser, "xml", data));
