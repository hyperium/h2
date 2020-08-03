use std::{io, str};
pub use tracing;
pub use tracing_subscriber;

pub fn init() -> tracing::dispatcher::DefaultGuard {
    tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
            .with_writer(PrintlnWriter { _p: () })
            .finish(),
    )
}

struct PrintlnWriter {
    _p: (),
}

impl tracing_subscriber::fmt::MakeWriter for PrintlnWriter {
    type Writer = PrintlnWriter;
    fn make_writer(&self) -> Self::Writer {
        PrintlnWriter { _p: () }
    }
}

impl io::Write for PrintlnWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let s = str::from_utf8(buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        println!("{}", s);
        Ok(s.len())
    }

    fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> io::Result<()> {
        println!("{}", fmt);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
