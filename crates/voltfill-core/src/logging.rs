//! VOLTFILL's OTLP JSON logs, written by `otel-log` for the VOLTFILL crates only.
use otel_log::opentelemetry_sdk::logs::SdkLoggerProvider;
use std::io::Write;

const SCOPE: &str = "voltfill";
const TARGETS: &[&str] = &["voltfill_core", "voltfill_cli", "voltfill_server"];

pub struct TelemetryLogger(otel_log::TelemetryLogger);
impl TelemetryLogger {
    pub fn new(
        service: &'static str,
        level: log::LevelFilter,
        writer: impl Write + Send + 'static,
    ) -> Self {
        Self(otel_log::TelemetryLogger::new(
            service, SCOPE, TARGETS, level, writer,
        ))
    }
    /// Supply a provider with an OTLP exporter to integrate a collector.
    pub fn with_provider(provider: SdkLoggerProvider, level: log::LevelFilter) -> Self {
        Self(otel_log::TelemetryLogger::with_provider(
            provider, SCOPE, TARGETS, level,
        ))
    }
    pub fn try_init(self) -> Result<(), log::SetLoggerError> {
        self.0.try_init()
    }
}
impl log::Log for TelemetryLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.0.enabled(metadata)
    }
    fn log(&self, record: &log::Record<'_>) {
        self.0.log(record)
    }
    fn flush(&self) {
        self.0.flush()
    }
}
