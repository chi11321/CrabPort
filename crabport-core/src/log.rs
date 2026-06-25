use chrono::Local;
use tracing_subscriber::fmt::{format::Writer, time::FormatTime};

struct LocalTime;

impl FormatTime for LocalTime {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{}", Local::now().format("%Y-%m-%d %H:%M:%S%.6f"))
    }
}

pub fn init() {
    tracing_subscriber::fmt().with_timer(LocalTime).init();
}
