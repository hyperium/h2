pub use tracing;
pub use tracing_subscriber;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init() -> tracing::dispatcher::DefaultGuard {
    let use_colors = atty::is(atty::Stream::Stdout);
    let layer = tracing_tree::HierarchicalLayer::default()
        .with_writer(tracing_subscriber::fmt::writer::TestWriter::default())
        .with_indent_lines(true)
        .with_ansi(use_colors)
        .with_targets(true)
        .with_indent_amount(2);

    tracing_subscriber::registry().with(layer).set_default()
}
