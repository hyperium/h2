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
use std::io::Cursor;
use std::path::Path;
use std::str;

const MAX_CHUNK: usize = 2 * 1024;

#[test]
fn hpack_failing_1() {
    FuzzHpack::new_reduced([
        14571479824075392697, 1933795017656710260, 3334564825787790363, 18384038562943935004,
    ], 100).run();
}

#[test]
fn hpack_failing_2() {
    FuzzHpack::new_reduced([
        93927840931624528, 7252171548136134810, 13289640692556535960, 11484086244506193733,
    ], 100).run();
}

#[test]
fn hpack_failing_3() {
    FuzzHpack::new_reduced([
        4320463360720445614, 7244328615656028238, 10856862580207993426, 5400459931473084625,
    ], 61).run();
}

#[test]
fn hpack_failing_4() {
    FuzzHpack::new_reduced([
        7199712575090753518, 8301132414711594706, 8069319383349578021, 5376546610900316263,
    ], 107).run();
}

#[test]
fn hpack_failing_5() {
    FuzzHpack::new_reduced([
        17764083779960581082, 12579311332935512090, 16627815831742045696, 13140603923739395199,
    ], 101).run();
}

#[test]
fn hpack_failing_6() {
    FuzzHpack::new_reduced([
        7970045195656406858, 7319095306567062282, 8226114865494971289, 10649653503082373659,
    ], 147).run();
}

#[test]
fn hpack_failing_7() {
    FuzzHpack::new_reduced([
        7990149962280599924, 6223290743332495022, 5461160958499241043, 157399552951946949,
    ], 31).run();
}

#[test]
fn hpack_failing_8() {
    FuzzHpack::new_reduced([
        5719253325816917205, 15546677577651198340, 11565363105171925122, 12844885905471928303,
    ], 110).run();
}

#[test]
fn hpack_fuzz() {
    fn prop(fuzz: FuzzHpack) -> TestResult {
        fuzz.run();
        TestResult::from_bool(true)
    }

    QuickCheck::new()
        .tests(100)
        .quickcheck(prop as fn(FuzzHpack) -> TestResult)
}

#[derive(Debug, Clone)]
struct FuzzHpack {
    // The magic seed that makes the test case reproducible
    seed: [usize; 4],

    // The set of headers to encode / decode
    frames: Vec<HeaderFrame>,

    // The list of chunk sizes to do it in
    chunks: Vec<usize>,

    // Number of times reduced
    reduced: usize,
}

#[derive(Debug, Clone)]
struct HeaderFrame {
    resizes: Vec<usize>,
    headers: Vec<Header<Option<HeaderName>>>,
}

impl FuzzHpack {
    fn new_reduced(seed: [usize; 4], i: usize) -> FuzzHpack {
        FuzzHpack::new(seed)
    }

    fn new(seed: [usize; 4]) -> FuzzHpack {
        // Seed the RNG
        let mut rng = StdRng::from_seed(&seed);

        // Generates a bunch of source headers
        let mut source: Vec<Header<Option<HeaderName>>> = vec![];

        for _ in 0..2000 {
            source.push(gen_header(&mut rng));
        }

        // Actual test run headers
        let num: usize = rng.gen_range(40, 500);

        let mut frames: Vec<HeaderFrame> = vec![];
        let mut added = 0;

        let skew: i32 = rng.gen_range(1, 5);

        // Rough number of headers to add
        while added < num {
            let mut frame = HeaderFrame {
                resizes: vec![],
                headers: vec![],
            };

            match rng.gen_range(0, 20) {
                0 => {
                    // Two resizes
                    let high = rng.gen_range(128, MAX_CHUNK * 2);
                    let low = rng.gen_range(0, high);

                    frame.resizes.extend(&[low, high]);
                }
                1...3 => {
                    frame.resizes.push(rng.gen_range(128, MAX_CHUNK * 2));
                }
                _ => {}
            }

            for _ in 0..rng.gen_range(1, (num - added) + 1) {
                added += 1;

                let x: f64 = rng.gen_range(0.0, 1.0);
                let x = x.powi(skew);

                let i = (x * source.len() as f64) as usize;
                frame.headers.push(source[i].clone());
            }

            frames.push(frame);
        }

        // Now, generate the buffer sizes used to encode
        let mut chunks = vec![];

        for _ in 0..rng.gen_range(0, 100) {
            chunks.push(rng.gen_range(0, MAX_CHUNK));
        }

        FuzzHpack {
            seed: seed,
            frames: frames,
            chunks: chunks,
            reduced: 0,
        }
    }

    fn run(mut self) {
        let mut chunks = self.chunks;
        let mut frames = self.frames;
        let mut expect = vec![];

        let mut encoder = Encoder::default();
        let mut decoder = Decoder::default();

        for frame in frames {
            expect.extend(frame.headers.clone());

            let mut index = None;
            let mut input = frame.headers.into_iter();

            let mut buf = BytesMut::with_capacity(
                chunks.pop().unwrap_or(MAX_CHUNK));

            if let Some(max) = frame.resizes.iter().max() {
                decoder.queue_size_update(*max);
            }

            // Apply resizes
            for resize in &frame.resizes {
                encoder.update_max_size(*resize);
            }

            loop {
                match encoder.encode(index.take(), &mut input, &mut buf) {
                    Encode::Full => break,
                    Encode::Partial(i) => {
                        index = Some(i);

                        // Decode the chunk!
                        decoder.decode(&mut Cursor::new(buf.into()), |e| {
                            assert_eq!(e, expect.remove(0).reify().unwrap());
                        }).unwrap();

                        buf = BytesMut::with_capacity(
                            chunks.pop().unwrap_or(MAX_CHUNK));
                    }
                }
            }

            // Decode the chunk!
            decoder.decode(&mut Cursor::new(buf.into()), |e| {
                assert_eq!(e, expect.remove(0).reify().unwrap());
            }).unwrap();
        }

        assert_eq!(0, expect.len());
    }
}

impl Arbitrary for FuzzHpack {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        FuzzHpack::new(quickcheck::Rng::gen(g))
    }
}

fn gen_header(g: &mut StdRng) -> Header<Option<HeaderName>> {
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

        Header::Field { name: Some(name), value: value }
    }
}

fn gen_header_name(g: &mut StdRng) -> HeaderName {
    use http::header;

    if g.gen_weighted_bool(2) {
        g.choose(&[
            header::ACCEPT,
            header::ACCEPT_CHARSET,
            header::ACCEPT_ENCODING,
            header::ACCEPT_LANGUAGE,
            header::ACCEPT_PATCH,
            header::ACCEPT_RANGES,
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            header::ACCESS_CONTROL_MAX_AGE,
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            header::ACCESS_CONTROL_REQUEST_METHOD,
            header::AGE,
            header::ALLOW,
            header::ALT_SVC,
            header::AUTHORIZATION,
            header::CACHE_CONTROL,
            header::CONNECTION,
            header::CONTENT_DISPOSITION,
            header::CONTENT_ENCODING,
            header::CONTENT_LANGUAGE,
            header::CONTENT_LENGTH,
            header::CONTENT_LOCATION,
            header::CONTENT_MD5,
            header::CONTENT_RANGE,
            header::CONTENT_SECURITY_POLICY,
            header::CONTENT_SECURITY_POLICY_REPORT_ONLY,
            header::CONTENT_TYPE,
            header::COOKIE,
            header::DNT,
            header::DATE,
            header::ETAG,
            header::EXPECT,
            header::EXPIRES,
            header::FORWARDED,
            header::FROM,
            header::HOST,
            header::IF_MATCH,
            header::IF_MODIFIED_SINCE,
            header::IF_NONE_MATCH,
            header::IF_RANGE,
            header::IF_UNMODIFIED_SINCE,
            header::LAST_MODIFIED,
            header::KEEP_ALIVE,
            header::LINK,
            header::LOCATION,
            header::MAX_FORWARDS,
            header::ORIGIN,
            header::PRAGMA,
            header::PROXY_AUTHENTICATE,
            header::PROXY_AUTHORIZATION,
            header::PUBLIC_KEY_PINS,
            header::PUBLIC_KEY_PINS_REPORT_ONLY,
            header::RANGE,
            header::REFERER,
            header::REFERRER_POLICY,
            header::REFRESH,
            header::RETRY_AFTER,
            header::SERVER,
            header::SET_COOKIE,
            header::STRICT_TRANSPORT_SECURITY,
            header::TE,
            header::TK,
            header::TRAILER,
            header::TRANSFER_ENCODING,
            header::TSV,
            header::USER_AGENT,
            header::UPGRADE,
            header::UPGRADE_INSECURE_REQUESTS,
            header::VARY,
            header::VIA,
            header::WARNING,
            header::WWW_AUTHENTICATE,
            header::X_CONTENT_TYPE_OPTIONS,
            header::X_DNS_PREFETCH_CONTROL,
            header::X_FRAME_OPTIONS,
            header::X_XSS_PROTECTION,
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

            decoder.decode(&mut Cursor::new(case.wire.clone().into()), |e| {
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
                Header::new(name.clone().into(), value.clone().into()).unwrap().into()
            }).collect();

            encoder.encode(None, &mut input.clone().into_iter(), &mut buf);

            decoder.decode(&mut Cursor::new(buf.into()), |e| {
                assert_eq!(e, input.remove(0).reify().unwrap());
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
