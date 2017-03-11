
/// Header frame
///
/// This could be either a request or a response.
pub struct Headers {
    stream_id: StreamId,
    headers: HeaderMap,
    pseudo: Pseudo,
}

pub struct Pseudo {
    // Request
    method: Option<()>,
    scheme: Option<()>,
    authority: Option<()>,
    path: Option<()>,

    // Response
    status: Option<()>,
}
