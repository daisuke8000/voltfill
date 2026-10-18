use serde_json::Value;
    pub fn otlp_event(envelope: Value) -> Value {
        fn decode(value: &Value) -> Value {
            if let Some(v) = value.get("stringValue") { return v.clone(); }
            if let Some(v) = value.get("boolValue") { return v.clone(); }
            if let Some(v) = value.get("intValue") { return serde_json::json!(v.as_str().unwrap().parse::<i64>().unwrap()); }
            if let Some(v) = value.get("doubleValue") { return v.clone(); }
            if let Some(v) = value.pointer("/arrayValue/values") { return Value::Array(v.as_array().unwrap().iter().map(decode).collect()); }
            if let Some(v) = value.pointer("/kvlistValue/values") { return Value::Object(v.as_array().unwrap().iter().map(|pair| (pair["key"].as_str().unwrap().to_owned(), decode(&pair["value"]))).collect()); }
            panic!("unexpected OTLP value: {value}")
        }
        let resource = &envelope["resourceLogs"][0];
        assert_eq!(resource["resource"]["attributes"][0]["key"], "service.name");
        let record = &resource["scopeLogs"][0]["logRecords"][0];
        assert!(record["timeUnixNano"].as_str().unwrap().parse::<u64>().unwrap() > 0);
        assert!(record["observedTimeUnixNano"].as_str().unwrap().parse::<u64>().unwrap() > 0);
        assert!(record["severityNumber"].as_u64().unwrap() > 0);
        let mut body = decode(&record["body"]);
        if !body.is_object() { body = serde_json::json!({"event":body}); }
        body["level"] = record["severityText"].clone();
        body["timestamp_ms"] = serde_json::json!(record["timeUnixNano"].as_str().unwrap().parse::<u64>().unwrap()/1_000_000);
        body
    }
