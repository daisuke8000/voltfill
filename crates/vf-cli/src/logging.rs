//! JSON Lines logs for the workspace crates only.
//!
//! Each record is one JSON object per line. A message that is itself a JSON
//! object is emitted with its fields; any other message becomes `event`.
//! `level`, `target`, `service` and `timestamp_ms` are added to every record.
use serde_json::{Map, Value};
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const TARGETS: &[&str] = &["vf_core", "vf_legacy", "vf_client", "vf_server", "vf_cli"];

pub struct JsonLogger {
    service: &'static str,
    level: log::LevelFilter,
    writer: Mutex<Box<dyn Write + Send>>,
}
impl JsonLogger {
    #[must_use]
    pub fn new(
        service: &'static str,
        level: log::LevelFilter,
        writer: impl Write + Send + 'static,
    ) -> Self {
        Self {
            service,
            level,
            writer: Mutex::new(Box::new(writer)),
        }
    }
    pub fn try_init(self) -> Result<(), log::SetLoggerError> {
        let level = self.level;
        log::set_boxed_logger(Box::new(self))?;
        log::set_max_level(level);
        Ok(())
    }
}
impl log::Log for JsonLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level
            && TARGETS.iter().any(|prefix| {
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
        let message = record.args().to_string();
        let mut event = match serde_json::from_str::<Value>(&message) {
            Ok(Value::Object(fields)) => fields,
            _ => Map::from_iter([("event".to_owned(), Value::String(message))]),
        };
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        event.insert("level".into(), record.level().as_str().into());
        event.insert("target".into(), record.target().into());
        event.insert("service".into(), self.service.into());
        event.insert("timestamp_ms".into(), timestamp_ms.into());
        if let Ok(mut writer) = self.writer.lock() {
            // Logging failure must not replace or discard the caller's result.
            let _ = serde_json::to_writer(&mut *writer, &Value::Object(event))
                .and_then(|()| writeln!(writer).map_err(serde_json::Error::io));
        }
    }
    fn flush(&self) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.flush();
        }
    }
}
