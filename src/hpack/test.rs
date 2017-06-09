extern crate bytes;
extern crate hex;
extern crate walkdir;
extern crate serde;
extern crate serde_json;
extern crate quickcheck;
extern crate rand;

use super::{Header, Decoder, Encoder, Encode};

use http::header::{HeaderName, HeaderValue};

use self::bytes::BytesMut;
use self::hex::FromHex;
use self::serde_json::Value;
use self::walkdir::WalkDir;
use self::quickcheck::{QuickCheck, Arbitrary, Gen, TestResult};
use self::rand::{StdRng, Rng, SeedableRng};

use std::env;
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::str;

const MAX_CHUNK: usize = 2 * 1024;

#[test]
fn hpack_fuzz_failing_1() {
    FuzzHpack::new_reduced([
        12433181738898983662,
        14102727336666980714,
        6105092270172216412,
        16258270543720336235,
    ], 1).run();
}

#[test]
fn hpack_fuzz() {
    fn prop(fuzz: FuzzHpack) -> TestResult {
        fuzz.run();
        TestResult::from_bool(true)
    }

    QuickCheck::new()
        .tests(5000)
        .quickcheck(prop as fn(FuzzHpack) -> TestResult)
}

#[derive(Debug, Clone)]
struct FuzzHpack {
    // The magic seed that makes the test case reproducible
    seed: [usize; 4],

    // The set of headers to encode / decode
    headers: Vec<Header>,

    // The list of chunk sizes to do it in
    chunks: Vec<usize>,

    // Number of times reduced
    reduced: usize,
}

impl FuzzHpack {
    fn new_reduced(seed: [usize; 4], i: usize) -> FuzzHpack {
        let mut ret = FuzzHpack::new(seed);
        ret.headers.drain(i..);
        ret
    }

    fn new(seed: [usize; 4]) -> FuzzHpack {
        // Seed the RNG
        let mut rng = StdRng::from_seed(&seed);

        // Generates a bunch of source headers
        let mut source: Vec<Header> = vec![];

        for _ in 0..2000 {
            source.push(gen_header(&mut rng));
        }

        // Actual test run headers
        let num: usize = rng.gen_range(40, 300);
        let mut actual: Vec<Header> = vec![];

        let skew: i32 = rng.gen_range(1, 5);

        while actual.len() < num {
            let x: f64 = rng.gen_range(0.0, 1.0);
            let x = x.powi(skew);

            let i = (x * source.len() as f64) as usize;
            actual.push(source[i].clone());
        }

        // Now, generate the buffer sizes used to encode
        let mut chunks = vec![];

        for _ in 0..rng.gen_range(0, 100) {
            chunks.push(rng.gen_range(0, MAX_CHUNK));
        }

        FuzzHpack {
            seed: seed,
            headers: actual,
            chunks: chunks,
            reduced: 0,
        }
    }

    fn run(mut self) {
        let mut chunks = self.chunks;
        let mut headers = self.headers;
        let mut expect = headers.clone();

        let mut encoded = vec![];
        let mut buf = BytesMut::with_capacity(
            chunks.pop().unwrap_or(MAX_CHUNK));

        let mut index = None;
        let mut input = headers.into_iter();

        let mut encoder = Encoder::default();
        let mut decoder = Decoder::default();

        loop {
            match encoder.encode(index.take(), &mut input, &mut buf).unwrap() {
                Encode::Full => break,
                Encode::Partial(i) => {
                    index = Some(i);
                    encoded.push(buf);
                    buf = BytesMut::with_capacity(
                        chunks.pop().unwrap_or(MAX_CHUNK));
                }
            }
        }

        // Decode
        for buf in encoded {
            decoder.decode(&buf.into(), |e| {
                assert_eq!(e, expect.remove(0));
            });
        }

        assert_eq!(0, expect.len());
    }
}

impl Arbitrary for FuzzHpack {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        FuzzHpack::new(quickcheck::Rng::gen(g))
    }

    fn shrink(&self) -> Box<Iterator<Item=FuzzHpack>> {
        let s = self.clone();

        let iter = (1..self.headers.len()).map(move |i| {
            let mut r = s.clone();
            r.headers.drain(i..);
            r.reduced = i;
            r
        });

        Box::new(iter)
    }
}

fn gen_header(g: &mut StdRng) -> Header {
    use http::StatusCode;
    use http::method::{self, Method};

    if g.gen_weighted_bool(10) {
        match g.next_u32() % 5 {
            0 => {
                let value = gen_string(g, 4, 20);
                Header::Authority(value.into())
            }
            1 => {
                let method = match g.next_u32() % 6 {
                    0 => method::GET,
                    1 => method::POST,
                    2 => method::PUT,
                    3 => method::PATCH,
                    4 => method::DELETE,
                    5 => {
                        let n: usize = g.gen_range(3, 7);
                        let bytes: Vec<u8> = (0..n).map(|_| {
                            g.choose(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ").unwrap().clone()
                        }).collect();

                        Method::from_bytes(&bytes).unwrap()
                    }
                    _ => unreachable!(),
                };

                Header::Method(method)
            }
            2 => {
                let value = match g.next_u32() % 2 {
                    0 => "http",
                    1 => "https",
                    _ => unreachable!(),
                };

                Header::Scheme(value.into())
            }
            3 => {
                let value = match g.next_u32() % 100 {
                    0 => "/".to_string(),
                    1 => "/index.html".to_string(),
                    _ => gen_string(g, 2, 20),
                };

                Header::Path(value.into())
            }
            4 => {
                let status = (g.gen::<u16>() % 500) + 100;

                Header::Status(StatusCode::from_u16(status).unwrap())
            }
            _ => unreachable!(),
        }
    } else {
        let name = gen_header_name(g);
        let mut value = gen_header_value(g);

        if g.gen_weighted_bool(30) {
            value.set_sensitive(true);
        }

        Header::Field { name: name, value: value }
    }
}

fn gen_header_name(g: &mut StdRng) -> HeaderName {
    use http::header;

    if g.gen_weighted_bool(2) {
        g.choose(&[
            // TODO: more headers
            header::ACCEPT,
        ]).unwrap().clone()
    } else {
        let value = gen_string(g, 1, 25);
        HeaderName::from_bytes(value.as_bytes()).unwrap()
    }
}

fn gen_header_value(g: &mut StdRng) -> HeaderValue {
    let value = gen_string(g, 0, 70);
    HeaderValue::try_from_bytes(value.as_bytes()).unwrap()
}

fn gen_string(g: &mut StdRng, min: usize, max: usize) -> String {
    let bytes: Vec<_> = (min..max).map(|_| {
        // Chars to pick from
        g.choose(b"ABCDEFGHIJKLMNOPQRSTUVabcdefghilpqrstuvwxyz----").unwrap().clone()
    }).collect();

    String::from_utf8(bytes).unwrap()
}

/*
impl Arbitrary for HeaderSet {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        let mut source: Vec<Header> = vec![];

        for _ in 0..2000 {
            source.push(Header::arbitrary(g));
        }

        // Actual headers
        let num: usize = g.gen_range(40, 300);
        let mut actual: Vec<Header> = vec![];

        let skew: i32 = g.gen_range(1, 5);

        while actual.len() < num {
            let x: f64 = g.gen_range(0.0, 1.0);
            let x = x.powi(skew);

            let i = (x * source.len() as f64) as usize;
            actual.push(source[i].clone());
        }

        HeaderSet {
            headers: actual,
        }
    }

    fn shrink(&self) -> Box<Iterator<Item=HeaderSet>> {
        let headers = self.headers.clone();

        let iter = (0..headers.len()+1).map(move |i| {
            HeaderSet { headers: headers[..0].to_vec() }
        });

        Box::new(iter)
    }
}

impl Arbitrary for Header {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        use http::StatusCode;
        use http::method::{self, Method};

        if g.gen_weighted_bool(10) {
            match g.next_u32() % 5 {
                0 => {
                    let value = String::arbitrary(g);
                    Header::Authority(value.into())
                }
                1 => {
                    let method = match g.next_u32() % 6 {
                        0 => method::GET,
                        1 => method::POST,
                        2 => method::PUT,
                        3 => method::PATCH,
                        4 => method::DELETE,
                        5 => {
                            let n = g.gen::<usize>() % 7;
                            let bytes: Vec<u8> = (0..n).map(|_| {
                                g.choose(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ").unwrap().clone()
                            }).collect();

                            Method::from_bytes(&bytes).unwrap()
                        }
                        _ => unreachable!(),
                    };

                    Header::Method(method)
                }
                2 => {
                    let value = match g.next_u32() % 2 {
                        0 => "http",
                        1 => "https",
                        _ => unreachable!(),
                    };

                    Header::Scheme(value.into())
                }
                3 => {
                    let value = match g.next_u32() % 100 {
                        0 => "/".to_string(),
                        1 => "/index.html".to_string(),
                        _ => String::arbitrary(g),
                    };

                    Header::Path(value.into())
                }
                4 => {
                    let status = (g.gen::<u16>() % 500) + 100;

                    Header::Status(StatusCode::from_u16(status).unwrap())
                }
                _ => unreachable!(),
            }
        } else {
            let mut name = HeaderName2::arbitrary(g);
            let mut value = HeaderValue2::arbitrary(g);

            if g.gen_weighted_bool(30) {
                value.0.set_sensitive(true);
            }

            Header::Field { name: name.0, value: value.0 }
        }
    }
}

#[derive(Clone)]
struct HeaderName2(HeaderName);

#[derive(Clone)]
struct HeaderValue2(HeaderValue);

impl Arbitrary for HeaderName2 {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        use http::header;

        if g.gen_weighted_bool(2) {
            g.choose(&[
                HeaderName2(header::ACCEPT),
            ]).unwrap().clone()
        } else {
            let len = g.gen::<usize>() % 25 + 1;

            let value: Vec<u8> = (0..len).map(|_| {
                g.choose(b"abcdefghijklmnopqrstuvwxyz-").unwrap().clone()
            }).collect();

            HeaderName2(HeaderName::from_bytes(&value).unwrap())
        }
    }
}

impl Arbitrary for HeaderValue2 {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        // Random length
        let len = g.gen::<usize>() % 70;

        // Generate the value
        let value: Vec<u8> = (0..len).map(|_| {
            g.choose(b"abcdefghijklmnopqrstuvwxyz -_").unwrap().clone()
        }).collect();

        HeaderValue2(HeaderValue::try_from_bytes(&value).unwrap())
    }
}
*/

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

        println!("");
        println!("");
        println!("~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~");
        println!("~~~ {:?} ~~~", entry.path());
        println!("~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~");
        println!("");
        println!("");

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

        // First, check decoding against the fixtures
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

        let mut encoder = Encoder::default();
        let mut decoder = Decoder::default();

        // Now, encode the headers
        for case in &cases {
            let mut buf = BytesMut::with_capacity(64 * 1024);

            if let Some(size) = case.header_table_size {
                encoder.update_max_size(size);
                decoder.queue_size_update(size);
            }

            let mut input: Vec<_> = case.expect.iter().map(|&(ref name, ref value)| {
                Header::new(name.clone().into(), value.clone().into()).unwrap()
            }).collect();

            encoder.encode(None, &mut input.clone().into_iter(), &mut buf).unwrap();

            decoder.decode(&buf.into(), |e| {
                assert_eq!(e, input.remove(0));
            }).unwrap();

            assert_eq!(0, input.len());
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
