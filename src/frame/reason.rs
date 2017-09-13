use std::fmt;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Reason {
    NoError,
    ProtocolError,
    InternalError,
    FlowControlError,
    SettingsTimeout,
    StreamClosed,
    FrameSizeError,
    RefusedStream,
    Cancel,
    CompressionError,
    ConnectError,
    EnhanceYourCalm,
    InadequateSecurity,
    Http11Required,
    Other(u32),
    // TODO: reserve additional variants
}

// ===== impl Reason =====

impl Reason {
    pub fn description(&self) -> &str {
        use self::Reason::*;

        match *self {
            NoError => "not a result of an error",
            ProtocolError => "unspecific protocol error detected",
            InternalError => "unexpected internal error encountered",
            FlowControlError => "flow-control protocol violated",
            SettingsTimeout => "settings ACK not received in timely manner",
            StreamClosed => "received frame when stream half-closed",
            FrameSizeError => "frame sent with invalid size",
            RefusedStream => "refused stream before processing any application logic",
            Cancel => "stream no longer needed",
            CompressionError => "unable to maintain the header compression context",
            ConnectError => {
                "connection established in response to a CONNECT request \
                 was reset or abnormally closed"
            },
            EnhanceYourCalm => "detected excessive load generating behavior",
            InadequateSecurity => "security properties do not meet minimum requirements",
            Http11Required => "endpoint requires HTTP/1.1",
            Other(_) => "other reason (ain't no tellin')",
        }
    }
}

impl From<u32> for Reason {
    fn from(src: u32) -> Reason {
        use self::Reason::*;

        match src {
            0x0 => NoError,
            0x1 => ProtocolError,
            0x2 => InternalError,
            0x3 => FlowControlError,
            0x4 => SettingsTimeout,
            0x5 => StreamClosed,
            0x6 => FrameSizeError,
            0x7 => RefusedStream,
            0x8 => Cancel,
            0x9 => CompressionError,
            0xa => ConnectError,
            0xb => EnhanceYourCalm,
            0xc => InadequateSecurity,
            0xd => Http11Required,
            _ => Other(src),
        }
    }
}

impl From<Reason> for u32 {
    fn from(src: Reason) -> u32 {
        use self::Reason::*;

        match src {
            NoError => 0x0,
            ProtocolError => 0x1,
            InternalError => 0x2,
            FlowControlError => 0x3,
            SettingsTimeout => 0x4,
            StreamClosed => 0x5,
            FrameSizeError => 0x6,
            RefusedStream => 0x7,
            Cancel => 0x8,
            CompressionError => 0x9,
            ConnectError => 0xa,
            EnhanceYourCalm => 0xb,
            InadequateSecurity => 0xc,
            Http11Required => 0xd,
            Other(v) => v,
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "{}", self.description())
    }
}
