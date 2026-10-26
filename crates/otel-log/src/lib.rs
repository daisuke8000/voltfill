//! OpenTelemetry SDK logs. Local output uses the official OTLP JSON mapping.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
use opentelemetry::InstrumentationScope;
use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::logs::v1::ResourceLogs;
use opentelemetry_proto::transform::common::tonic::ResourceAttributesWithSchema;
pub use opentelemetry_sdk;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{LogProcessor, SdkLogRecord, SdkLogger, SdkLoggerProvider};
use serde_json::Value;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

struct JsonProcessor {
    writer: Mutex<Box<dyn Write + Send>>,
    resource: ResourceAttributesWithSchema,
}
impl std::fmt::Debug for JsonProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OtlpJsonProcessor")
    }
}
impl LogProcessor for JsonProcessor {
    fn emit(&self, record: &mut SdkLogRecord, scope: &InstrumentationScope) {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs::from(((&*record, scope), &self.resource))],
        };
        if let Ok(mut writer) = self.writer.lock() {
            // Logging failure must not replace or discard the caller's result.
            let _ = serde_json::to_writer(&mut *writer, &request)
                .and_then(|_| writeln!(writer).map_err(serde_json::Error::io));
        }
    }
    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.writer
            .lock()
            .map_err(|_| {
                opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                    "log writer poisoned".into(),
                )
            })?
            .flush()
            .map_err(|e| opentelemetry_sdk::error::OTelSdkError::InternalFailure(e.to_string()))
    }
    fn shutdown_with_timeout(&self, _: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        self.force_flush()
    }
    fn set_resource(&mut self, resource: &Resource) {
        self.resource = resource.into();
    }
}

pub struct TelemetryLogger {
    provider: SdkLoggerProvider,
    logger: SdkLogger,
    level: log::LevelFilter,
    targets: &'static [&'static str],
}
impl TelemetryLogger {
    /// Emits records whose `log` target is one of `targets` or a module under it,
    /// under the instrumentation scope `scope`; every other record is dropped.
    #[must_use]
    pub fn new(
        service: &'static str,
        scope: &'static str,
        targets: &'static [&'static str],
        level: log::LevelFilter,
        writer: impl Write + Send + 'static,
    ) -> Self {
        let provider = SdkLoggerProvider::builder()
            .with_resource(Resource::builder_empty().with_service_name(service).build())
            .with_log_processor(JsonProcessor {
                writer: Mutex::new(Box::new(writer)),
                resource: ResourceAttributesWithSchema::default(),
            })
            .build();
        Self::with_provider(provider, scope, targets, level)
    }
    /// Supply a provider with an OTLP exporter to integrate a collector.
    #[must_use]
    pub fn with_provider(
        provider: SdkLoggerProvider,
        scope: &'static str,
        targets: &'static [&'static str],
        level: log::LevelFilter,
    ) -> Self {
        let logger = provider.logger(scope);
        Self {
            provider,
            logger,
            level,
            targets,
        }
    }
    pub fn try_init(self) -> Result<(), log::SetLoggerError> {
        let level = self.level;
        log::set_boxed_logger(Box::new(self))?;
        log::set_max_level(level);
        Ok(())
    }
}
use log::Log;
impl Log for TelemetryLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
            && self.targets.iter().any(|prefix| {
                metadata.target() == *prefix
                    || metadata
                        .target()
                        .strip_prefix(prefix)
                        .is_some_and(|tail| tail.starts_with("::"))
            })
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut data = self.logger.create_log_record();
        let severity = match record.level() {
            log::Level::Error => Severity::Error,
            log::Level::Warn => Severity::Warn,
            log::Level::Info => Severity::Info,
            log::Level::Debug => Severity::Debug,
            log::Level::Trace => Severity::Trace,
        };
        data.set_timestamp(SystemTime::now());
        data.set_severity_number(severity);
        data.set_severity_text(record.level().as_str());
        data.set_target(record.target().to_owned());
        let message = record.args().to_string();
        data.set_body(
            serde_json::from_str::<Value>(&message)
                .ok()
                .and_then(any_value)
                .unwrap_or_else(|| AnyValue::String(message.into())),
        );
        self.logger.emit(data);
    }
    fn flush(&self) {
        let _ = self.provider.force_flush();
    }
}
fn any_value(value: Value) -> Option<AnyValue> {
    Some(match value {
        Value::Null => return None,
        Value::Bool(v) => AnyValue::Boolean(v),
        Value::Number(v) => {
            if let Some(v) = v.as_i64() {
                AnyValue::Int(v)
            } else if v.is_u64() {
                AnyValue::String(v.to_string().into())
            } else {
                AnyValue::Double(v.as_f64()?)
            }
        }
        Value::String(v) => AnyValue::String(v.into()),
        Value::Array(v) => {
            AnyValue::ListAny(Box::new(v.into_iter().filter_map(any_value).collect()))
        }
        Value::Object(v) => AnyValue::Map(Box::new(
            v.into_iter()
                .filter_map(|(k, v)| any_value(v).map(|v| (k.into(), v)))
                .collect(),
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::Context;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };
    use std::sync::Arc;
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("the test capture writer must not have panicked")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn official_otlp_mapping_retains_trace_context_and_typed_event_body() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = TelemetryLogger::new(
            "test",
            "test",
            &["test_target"],
            log::LevelFilter::Info,
            Capture(bytes.clone()),
        );
        let context = Context::new().with_remote_span_context(SpanContext::new(
            TraceId::from(123),
            SpanId::from(456),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        ));
        let _guard = context.attach();
        logger.log(
            &log::Record::builder()
                .args(format_args!(
                    "{}",
                    serde_json::json!({"event":"typed","count":7,"wait":false})
                ))
                .level(log::Level::Info)
                .target("test_target")
                .build(),
        );
        logger.flush();
        let bytes = bytes
            .lock()
            .expect("the test capture writer must not have panicked");
        let request: ExportLogsServiceRequest = serde_json::from_slice(&bytes)
            .expect("logger output must deserialize as an OTLP export request");
        let record = &request.resource_logs[0].scope_logs[0].log_records[0];
        assert_eq!(record.severity_number, 9);
        assert!(
            record.time_unix_nano > 0 && record.observed_time_unix_nano >= record.time_unix_nano
        );
        assert_eq!(record.trace_id, 123u128.to_be_bytes());
        assert_eq!(record.span_id, 456u64.to_be_bytes());
        let json: Value =
            serde_json::from_slice(&bytes).expect("logger output must also be valid JSON");
        let record = &json["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(record["traceId"], "0000000000000000000000000000007b");
        assert!(
            record["body"]["kvlistValue"]["values"]
                .as_array()
                .expect("an OTLP map body must contain a values array")
                .iter()
                .any(|p| p["key"] == "count" && p["value"]["intValue"] == "7")
        );
    }
}
