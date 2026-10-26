use serde_json::Value;

pub fn otlp_event(envelope: Value) -> Value {
    fn decode(value: &Value) -> Value {
        if let Some(value) = value.get("stringValue") {
            return value.clone();
        }
        if let Some(value) = value.get("boolValue") {
            return value.clone();
        }
        if let Some(value) = value.get("intValue") {
            let number: i64 = value
                .as_str()
                .expect("OTLP JSON encodes intValue as a decimal string")
                .parse()
                .expect("intValue must fit in a signed 64-bit integer");
            return serde_json::json!(number);
        }
        if let Some(value) = value.get("doubleValue") {
            return value.clone();
        }
        if let Some(values) = value.pointer("/arrayValue/values") {
            return Value::Array(
                values
                    .as_array()
                    .expect("arrayValue.values must be an array")
                    .iter()
                    .map(decode)
                    .collect(),
            );
        }
        if let Some(values) = value.pointer("/kvlistValue/values") {
            return Value::Object(
                values
                    .as_array()
                    .expect("kvlistValue.values must be an array")
                    .iter()
                    .map(|pair| {
                        (
                            pair["key"]
                                .as_str()
                                .expect("OTLP attribute keys must be strings")
                                .to_owned(),
                            decode(&pair["value"]),
                        )
                    })
                    .collect(),
            );
        }
        panic!("unexpected OTLP value: {value}")
    }

    fn timestamp(value: &Value) -> u64 {
        value
            .as_str()
            .expect("OTLP timestamps must be decimal strings")
            .parse()
            .expect("OTLP timestamps must fit in unsigned 64-bit nanoseconds")
    }

    let resource = &envelope["resourceLogs"][0];
    assert_eq!(resource["resource"]["attributes"][0]["key"], "service.name");
    let record = &resource["scopeLogs"][0]["logRecords"][0];
    let time = timestamp(&record["timeUnixNano"]);
    assert!(time > 0, "log record must have an emission timestamp");
    assert!(
        timestamp(&record["observedTimeUnixNano"]) > 0,
        "log record must have an observation timestamp"
    );
    assert!(
        record["severityNumber"]
            .as_u64()
            .expect("severityNumber must be an unsigned integer")
            > 0,
        "logger must populate severity"
    );
    let mut body = decode(&record["body"]);
    if !body.is_object() {
        body = serde_json::json!({"event":body});
    }
    body["level"] = record["severityText"].clone();
    body["timestamp_ms"] = serde_json::json!(time / 1_000_000);
    body
}
