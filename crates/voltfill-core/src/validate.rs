use crate::schema::{Normalization, OutputType, Validator};
use serde_json::Value;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("value does not satisfy declared type")]
    Type,
    #[error("value does not satisfy validator")]
    Rule,
}

pub fn normalize(raw: &str, normalization: Normalization) -> String {
    match normalization {
        Normalization::None => raw.to_owned(),
        Normalization::Trim => raw.trim().to_owned(),
        Normalization::CollapseWhitespace => raw.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

fn iso_date(s: &str) -> bool {
    if s.len() != 10
        || s.as_bytes()[4] != b'-'
        || s.as_bytes()[7] != b'-'
        || s.bytes()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return false;
    }
    let [year, month, day] = [&s[..4], &s[5..7], &s[8..]].map(|part| {
        part.bytes()
            .fold(0, |n, digit| n * 10 + u32::from(digit - b'0'))
    });
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days).contains(&day)
}

fn decimal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let mut pieces = s.split('.');
    let whole = pieces.next().unwrap_or("");
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || (whole.len() > 1 && whole.starts_with('0'))
    {
        return false;
    }
    match pieces.next() {
        None => true,
        Some(fraction) => {
            !fraction.is_empty()
                && fraction.bytes().all(|b| b.is_ascii_digit())
                && pieces.next().is_none()
        }
    }
}

pub fn value_type(value: &Value, output: OutputType) -> Result<(), ValidationError> {
    let good = match output {
        OutputType::String | OutputType::Enum => value.is_string(),
        OutputType::StringArray => value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string)),
        OutputType::Date => value.as_str().is_some_and(iso_date),
        OutputType::Decimal => value.as_str().is_some_and(decimal),
        OutputType::Bool => value.is_boolean(),
    };
    if good {
        Ok(())
    } else {
        Err(ValidationError::Type)
    }
}

pub fn validate(
    value: &Value,
    output: OutputType,
    validators: &[Validator],
) -> Result<(), ValidationError> {
    value_type(value, output)?;
    for rule in validators {
        let good = match rule {
            Validator::NonEmpty => match value {
                Value::Array(items) => {
                    !items.is_empty()
                        && items
                            .iter()
                            .all(|item| item.as_str().is_some_and(|s| !s.trim().is_empty()))
                }
                _ => value.as_str().is_none_or(|s| !s.trim().is_empty()),
            },
            Validator::MaxChars { count } => {
                let within_limit =
                    |item: &Value| item.as_str().is_some_and(|s| s.chars().count() <= *count);
                match value {
                    Value::Array(items) => items.iter().all(within_limit),
                    _ => within_limit(value),
                }
            }
            Validator::OneOf { values } => values.contains(value),
        };
        if !good {
            return Err(ValidationError::Rule);
        }
    }
    Ok(())
}
