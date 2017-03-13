use tower::http::{HeaderName, Method, StatusCode, Str};

/// HPack table entry
pub enum Entry {
    Header {
        name: HeaderName,
        value: Str,
    },
    Authority(Str),
    Method(Method),
    Scheme(Str),
    Path(Str),
    Status(StatusCode),
}

impl Entry {
    pub fn len(&self) -> usize {
        match *self {
            Entry::Header { ref name, ref value } => {
                let n: &str = name.as_ref();
                32 + n.len() + value.len()
            }
            Entry::Authority(ref v) => {
                32 + 10 + v.len()
            }
            Entry::Method(ref v) => {
                32 + 7 + v.as_ref().len()
            }
            Entry::Scheme(ref v) => {
                32 + 7 + v.len()
            }
            Entry::Path(ref v) => {
                32 + 5 + v.len()
            }
            Entry::Status(ref v) => {
                32 + 7 + 3
            }
        }
    }
}
