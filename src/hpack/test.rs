extern crate hex;
extern crate walkdir;
extern crate serde;
extern crate serde_json;

use super::{Header, Decoder};

use self::hex::FromHex;
use self::serde_json::Value;
use self::walkdir::WalkDir;

use std::env;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;

#[test]
fn hpack_fixtures() {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/hpack");

    for entry in WalkDir::new(fixture_dir) {
        let entry = entry.unwrap();
        let path = entry.path().to_str().unwrap();

        if !path.ends_with(".json") {
            continue;
        }

        if path.contains("raw-data") {
            continue;
        }

        if let Some(filter) = env::var("HPACK_FIXTURE_FILTER").ok() {
            if !path.contains(&filter) {
                continue;
            }
        }

        test_fixture(entry.path());
    }
}

fn test_fixture(path: &Path) {
    let mut file = File::open(path).unwrap();
    let mut data = String::new();
    file.read_to_string(&mut data).unwrap();

    let story: Value = serde_json::from_str(&data).unwrap();
    test_story(story);
}

fn test_story(story: Value) {
    let story = story.as_object().unwrap();

    if let Some(cases) = story.get("cases") {
        let mut cases: Vec<_> = cases.as_array().unwrap().iter()
            .map(|case| {
                let case = case.as_object().unwrap();

                let size = case.get("header_table_size")
                    .map(|v| v.as_u64().unwrap() as usize);

                let wire = case.get("wire").unwrap().as_str().unwrap();
                let wire: Vec<u8> = FromHex::from_hex(wire.as_bytes()).unwrap();

                let mut expect: Vec<_> = case.get("headers").unwrap()
                    .as_array().unwrap().iter()
                    .map(|h| {
                        let h = h.as_object().unwrap();
                        let (name, val) = h.iter().next().unwrap();
                        (name.clone(), val.as_str().unwrap().to_string())
                    })
                    .collect();

                Case {
                    seqno: case.get("seqno").unwrap().as_u64().unwrap(),
                    wire: wire,
                    expect: expect,
                    header_table_size: size,
                }
            })
            .collect();

        cases.sort_by_key(|c| c.seqno);

        let mut decoder = Decoder::default();

        for case in &cases {
            let mut expect = case.expect.clone();

            if let Some(size) = case.header_table_size {
                decoder.queue_size_update(size);
            }

            decoder.decode(&case.wire.clone().into(), |e| {
                let (name, value) = expect.remove(0);
                assert_eq!(name, key_str(&e));
                assert_eq!(value, value_str(&e));
            }).unwrap();

            assert_eq!(0, expect.len());
        }
    }
}

struct Case {
    seqno: u64,
    wire: Vec<u8>,
    expect: Vec<(String, String)>,
    header_table_size: Option<usize>,
}

fn key_str(e: &Header) -> &str {
    match *e {
        Header::Field { ref name, .. } => name.as_str(),
        Header::Authority(..) => ":authority",
        Header::Method(..) => ":method",
        Header::Scheme(..) => ":scheme",
        Header::Path(..) => ":path",
        Header::Status(..) => ":status",
    }
}

fn value_str(e: &Header) -> &str {
    match *e {
        Header::Field { ref value, .. } => value.to_str().unwrap(),
        Header::Authority(ref v) => &**v,
        Header::Method(ref m) => m.as_str(),
        Header::Scheme(ref v) => &**v,
        Header::Path(ref v) => &**v,
        Header::Status(ref v) => v.as_str(),
    }
}
