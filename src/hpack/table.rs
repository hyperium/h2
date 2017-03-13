use hpack::Entry;

use tower::http::{HeaderName, StatusCode, Method, Str};
use std::collections::VecDeque;

/// Get an entry from the static table
pub fn get_static(idx: usize) -> Entry {
    use tower::http::StandardHeader::*;

    match idx {
        1 => Entry::Authority(Str::new()),
        2 => Entry::Method(Method::Get),
        3 => Entry::Method(Method::Post),
        4 => Entry::Path(Str::from_static("/")),
        5 => Entry::Path(Str::from_static("/index.html")),
        6 => Entry::Scheme(Str::from_static("http")),
        7 => Entry::Scheme(Str::from_static("https")),
        8 => Entry::Status(StatusCode::Ok),
        9 => Entry::Status(StatusCode::NoContent),
        10 => Entry::Status(StatusCode::PartialContent),
        11 => Entry::Status(StatusCode::NotModified),
        12 => Entry::Status(StatusCode::BadRequest),
        13 => Entry::Status(StatusCode::NotFound),
        14 => Entry::Status(StatusCode::InternalServerError),
        15 => Entry::Header {
            name: AcceptCharset.into(),
            value: Str::new(),
        },
        16 => Entry::Header {
            name: AcceptEncoding.into(),
            value: Str::from_static("gzip, deflate"),
        },
        17 => Entry::Header {
            name: AcceptLanguage.into(),
            value: Str::new(),
        },
        18 => Entry::Header {
            name: AcceptRanges.into(),
            value: Str::new(),
        },
        19 => Entry::Header {
            name: Accept.into(),
            value: Str::new(),
        },
        20 => Entry::Header {
            name: AccessControlAllowOrigin.into(),
            value: Str::new(),
        },
        21 => Entry::Header {
            name: Age.into(),
            value: Str::new(),
        },
        22 => Entry::Header {
            name: Allow.into(),
            value: Str::new(),
        },
        23 => Entry::Header {
            name: Authorization.into(),
            value: Str::new(),
        },
        24 => Entry::Header {
            name: CacheControl.into(),
            value: Str::new(),
        },
        25 => Entry::Header {
            name: ContentDisposition.into(),
            value: Str::new(),
        },
        26 => Entry::Header {
            name: ContentEncoding.into(),
            value: Str::new(),
        },
        27 => Entry::Header {
            name: ContentLanguage.into(),
            value: Str::new(),
        },
        28 => Entry::Header {
            name: ContentLength.into(),
            value: Str::new(),
        },
        29 => Entry::Header {
            name: ContentLocation.into(),
            value: Str::new(),
        },
        30 => Entry::Header {
            name: ContentRange.into(),
            value: Str::new(),
        },
        31 => Entry::Header {
            name: ContentType.into(),
            value: Str::new(),
        },
        32 => Entry::Header {
            name: Cookie.into(),
            value: Str::new(),
        },
        33 => Entry::Header {
            name: Date.into(),
            value: Str::new(),
        },
        34 => Entry::Header {
            name: Etag.into(),
            value: Str::new(),
        },
        35 => Entry::Header {
            name: Expect.into(),
            value: Str::new(),
        },
        36 => Entry::Header {
            name: Expires.into(),
            value: Str::new(),
        },
        37 => Entry::Header {
            name: From.into(),
            value: Str::new(),
        },
        38 => Entry::Header {
            name: Host.into(),
            value: Str::new(),
        },
        39 => Entry::Header {
            name: IfMatch.into(),
            value: Str::new(),
        },
        40 => Entry::Header {
            name: IfModifiedSince.into(),
            value: Str::new(),
        },
        41 => Entry::Header {
            name: IfNoneMatch.into(),
            value: Str::new(),
        },
        42 => Entry::Header {
            name: IfRange.into(),
            value: Str::new(),
        },
        43 => Entry::Header {
            name: IfUnmodifiedSince.into(),
            value: Str::new(),
        },
        44 => Entry::Header {
            name: LastModified.into(),
            value: Str::new(),
        },
        45 => Entry::Header {
            name: Link.into(),
            value: Str::new(),
        },
        46 => Entry::Header {
            name: Location.into(),
            value: Str::new(),
        },
        47 => Entry::Header {
            name: MaxForwards.into(),
            value: Str::new(),
        },
        48 => Entry::Header {
            name: ProxyAuthenticate.into(),
            value: Str::new(),
        },
        49 => Entry::Header {
            name: ProxyAuthorization.into(),
            value: Str::new(),
        },
        50 => Entry::Header {
            name: Range.into(),
            value: Str::new(),
        },
        51 => Entry::Header {
            name: Referer.into(),
            value: Str::new(),
        },
        52 => Entry::Header {
            name: Refresh.into(),
            value: Str::new(),
        },
        53 => Entry::Header {
            name: RetryAfter.into(),
            value: Str::new(),
        },
        54 => Entry::Header {
            name: Server.into(),
            value: Str::new(),
        },
        55 => Entry::Header {
            name: SetCookie.into(),
            value: Str::new(),
        },
        56 => Entry::Header {
            name: StrictTransportSecurity.into(),
            value: Str::new(),
        },
        57 => Entry::Header {
            name: TransferEncoding.into(),
            value: Str::new(),
        },
        58 => Entry::Header {
            name: UserAgent.into(),
            value: Str::new(),
        },
        59 => Entry::Header {
            name: Vary.into(),
            value: Str::new(),
        },
        60 => Entry::Header {
            name: Via.into(),
            value: Str::new(),
        },
        61 => Entry::Header {
            name: WwwAuthenticate.into(),
            value: Str::new(),
        },
        _ => unreachable!(),
    }
}

/*
pub struct Table {
    entries: VecDeque<HeaderPair>,
    max_size: usize,
}

pub enum Entry {
    Header {
        name: HeaderName,
        value: Str,
    },
    Authority(Str),
    Scheme(Str),
    Path(Str),
    Status(StatusCode),
}
impl Table {
    pub fn new(max_size: usize) -> Table {
        Table {
            entries: VecDeque::new(),
            max_size: max_size,
        }
    }
}

impl Default for Table {
    fn default() -> Table {
        // Default maximum size from the HTTP/2.0 spec.
        Table::new(4_096)
    }
}
*/
