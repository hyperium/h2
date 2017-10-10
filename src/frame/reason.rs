use std::fmt;


#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Reason(u32);

impl Reason {
    pub const NO_ERROR: Reason = Reason(0);
    pub const PROTOCOL_ERROR: Reason = Reason(1);
    pub const INTERNAL_ERROR: Reason = Reason(2);
    pub const FLOW_CONTROL_ERROR: Reason = Reason(3);
    pub const SETTINGS_TIMEOUT: Reason = Reason(4);
    pub const STREAM_CLOSED: Reason = Reason(5);
    pub const FRAME_SIZE_ERROR: Reason = Reason(6);
    pub const REFUSED_STREAM: Reason = Reason(7);
    pub const CANCEL: Reason = Reason(8);
    pub const COMPRESSION_ERROR: Reason = Reason(9);
    pub const CONNECT_ERROR: Reason = Reason(10);
    pub const ENHANCE_YOUR_CALM: Reason = Reason(11);
    pub const INADEQUATE_SECURITY: Reason = Reason(12);
    pub const HTTP11_REQUIRED: Reason = Reason(13);

    pub fn description(&self) -> &str {
        match self.0 {
            0 => "not a result of an error",
            1 => "unspecific protocol error detected",
            2 => "unexpected internal error encountered",
            3 => "flow-control protocol violated",
            4 => "settings ACK not received in timely manner",
            5 => "received frame when stream half-closed",
            6 => "frame with invalid size",
            7 => "refused stream before processing any application logic",
            8 => "stream no longer needed",
            9 => "unable to maintain the header compression context",
            10 => {
                "connection established in response to a CONNECT request was reset or abnormally \
                 closed"
            },
            11 => "detected excessive load generating behavior",
            12 => "security properties do not meet minimum requirements",
            13 => "endpoint requires HTTP/1.1",
            _ => "unknown reason",
        }
    }
}

impl From<u32> for Reason {
    fn from(src: u32) -> Reason {
        Reason(src)
    }
}

impl From<Reason> for u32 {
    fn from(src: Reason) -> u32 {
        src.0
    }
}

impl fmt::Debug for Reason {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self.0 {
            0 => "NO_ERROR",
            1 => "PROTOCOL_ERROR",
            2 => "INTERNAL_ERROR",
            3 => "FLOW_CONTROL_ERROR",
            4 => "SETTINGS_TIMEOUT",
            5 => "STREAM_CLOSED",
            6 => "FRAME_SIZE_ERROR",
            7 => "REFUSED_STREAM",
            8 => "CANCEL",
            9 => "COMPRESSION_ERROR",
            10 => "CONNECT_ERROR",
            11 => "ENHANCE_YOUR_CALM",
            12 => "INADEQUATE_SECURITY",
            13 => "HTTP_1_1_REQUIRED",
            other => return f.debug_tuple("Reason")
                .field(&Hex(other))
                .finish(),
        };
        f.write_str(name)
    }
}

struct Hex(u32);

impl fmt::Debug for Hex {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{}", self.description())
    }
}
