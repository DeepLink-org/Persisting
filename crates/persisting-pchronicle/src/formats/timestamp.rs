use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::{InputIssue, InputResult};

/// A semantic UTC timestamp together with its authoritative JSON scalar.
///
/// The scalar is retained so same-format recovery can distinguish numeric Unix
/// seconds from textual RFC3339 without relegating that distinction to
/// `unknown_fields` or format-specific `extra` data.
#[derive(Debug, Clone, PartialEq)]
pub struct StorylineTimestamp {
    instant: DateTime<Utc>,
    unix_nanos: i64,
    source: Value,
}

impl StorylineTimestamp {
    pub fn from_json(source: Value) -> InputResult<Self> {
        let instant = match &source {
            Value::String(value) => DateTime::parse_from_rfc3339(value)
                .map_err(|_| InputIssue::invalid("timestamp string must be RFC3339"))?
                .with_timezone(&Utc),
            Value::Number(value) => {
                let nanos = decimal_seconds_to_nanos(&value.to_string())?;
                DateTime::<Utc>::from_timestamp_nanos(nanos)
            }
            _ => {
                return Err(InputIssue::invalid(
                    "timestamp must be an RFC3339 string or Unix-seconds number",
                ));
            }
        };
        let unix_nanos = instant
            .timestamp_nanos_opt()
            .ok_or_else(|| InputIssue::invalid("timestamp is outside nanosecond range"))?;
        Ok(Self {
            instant,
            unix_nanos,
            source,
        })
    }

    pub fn from_rfc3339(value: &str) -> InputResult<Self> {
        Self::from_json(Value::String(value.to_string()))
    }

    pub fn from_utc(instant: DateTime<Utc>) -> InputResult<Self> {
        let unix_nanos = instant
            .timestamp_nanos_opt()
            .ok_or_else(|| InputIssue::invalid("timestamp is outside nanosecond range"))?;
        let source = Value::String(instant.to_rfc3339_opts(SecondsFormat::AutoSi, true));
        Ok(Self {
            instant,
            unix_nanos,
            source,
        })
    }

    pub fn instant(&self) -> DateTime<Utc> {
        self.instant
    }

    pub fn timestamp_nanos(&self) -> i64 {
        self.unix_nanos
    }

    pub fn canonical_rfc3339(&self) -> String {
        self.instant.to_rfc3339_opts(SecondsFormat::AutoSi, true)
    }

    pub fn source_value(&self) -> &Value {
        &self.source
    }

    pub fn source_string(&self) -> Option<&str> {
        self.source.as_str()
    }

    pub fn source_string_or_canonical(&self) -> String {
        self.source_string()
            .map(str::to_owned)
            .unwrap_or_else(|| self.canonical_rfc3339())
    }
}

impl Serialize for StorylineTimestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.source.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StorylineTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let source = Value::deserialize(deserializer)?;
        Self::from_json(source).map_err(serde::de::Error::custom)
    }
}

fn decimal_seconds_to_nanos(input: &str) -> InputResult<i64> {
    let (negative, unsigned) = match input.strip_prefix('-') {
        Some(value) => (true, value),
        None => (false, input),
    };
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map(|(mantissa, exponent)| {
            exponent
                .parse::<i32>()
                .map(|exponent| (mantissa, exponent))
                .map_err(|_| InputIssue::invalid("timestamp number has an invalid exponent"))
        })
        .transpose()?
        .unwrap_or((unsigned, 0));
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(InputIssue::invalid("timestamp number is invalid"));
    }

    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    let nanos_exponent =
        exponent
            .checked_sub(i32::try_from(fraction.len()).map_err(|_| {
                InputIssue::invalid("timestamp number has too many fractional digits")
            })?)
            .and_then(|value| value.checked_add(9))
            .ok_or_else(|| InputIssue::invalid("timestamp number exponent is out of range"))?;

    let magnitude = if nanos_exponent >= 0 {
        let mut value = parse_digits(&digits)?;
        for _ in 0..nanos_exponent {
            value = value
                .checked_mul(10)
                .ok_or_else(|| InputIssue::invalid("timestamp is outside nanosecond range"))?;
        }
        value
    } else {
        let remove = usize::try_from(-i64::from(nanos_exponent))
            .map_err(|_| InputIssue::invalid("timestamp number exponent is out of range"))?;
        let split = digits.len().saturating_sub(remove);
        if digits.as_bytes()[split..].iter().any(|byte| *byte != b'0') {
            return Err(InputIssue::invalid(
                "timestamp has precision finer than one nanosecond",
            ));
        }
        if split == 0 {
            0
        } else {
            parse_digits(&digits[..split])?
        }
    };
    let signed = if negative {
        magnitude
            .checked_neg()
            .ok_or_else(|| InputIssue::invalid("timestamp is outside nanosecond range"))?
    } else {
        magnitude
    };
    i64::try_from(signed).map_err(|_| InputIssue::invalid("timestamp is outside nanosecond range"))
}

fn parse_digits(digits: &str) -> InputResult<i128> {
    digits
        .parse::<i128>()
        .map_err(|_| InputIssue::invalid("timestamp is outside nanosecond range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn nanos_as_decimal(nanos: i64) -> String {
        let negative = nanos < 0;
        let magnitude = (nanos as i128).abs();
        let whole = magnitude / 1_000_000_000;
        let fraction = magnitude % 1_000_000_000;
        let sign = if negative { "-" } else { "" };
        if fraction == 0 {
            format!("{sign}{whole}")
        } else {
            let fraction = format!("{fraction:09}");
            format!("{sign}{whole}.{}", fraction.trim_end_matches('0'))
        }
    }

    proptest! {
        #[test]
        fn decimal_seconds_preserve_every_representable_nanosecond(nanos in any::<i64>()) {
            let encoded = nanos_as_decimal(nanos);
            prop_assert_eq!(decimal_seconds_to_nanos(&encoded).unwrap(), nanos);
        }

        #[test]
        fn integer_json_timestamps_preserve_their_semantic_nanoseconds(
            seconds in -9_000_000_000i64..=9_000_000_000,
        ) {
            let nanos = seconds * 1_000_000_000;
            let encoded = nanos_as_decimal(nanos);
            let source: Value = serde_json::from_str(&encoded).unwrap();
            let timestamp = StorylineTimestamp::from_json(source.clone()).unwrap();
            prop_assert_eq!(timestamp.timestamp_nanos(), nanos);
            prop_assert_eq!(serde_json::to_value(&timestamp).unwrap(), source);
        }

        #[test]
    fn finer_than_nanosecond_precision_is_rejected(
            whole in 0u64..=9_000_000_000,
            fraction in 0u32..=999_999_999,
            extra in 1u8..=9,
        ) {
            let encoded = format!("{whole}.{fraction:09}{extra}");
            prop_assert!(decimal_seconds_to_nanos(&encoded).is_err());
        }

        #[test]
        fn json_decimal_parsing_preserves_nanoseconds_without_float_rounding(
            whole in 0u64..=90_000_000,
            fraction in 1u32..=999_999_999,
        ) {
            let encoded = format!("-{whole}.{fraction:09}");
            let source: Value = serde_json::from_str(&encoded).unwrap();
            let timestamp = StorylineTimestamp::from_json(source).unwrap();
            let expected = -((whole as i128) * 1_000_000_000 + fraction as i128);
            prop_assert_eq!(timestamp.timestamp_nanos() as i128, expected);
        }

        #[test]
        fn canonical_rfc3339_roundtrips_the_instant(nanos in any::<i64>()) {
            let instant = DateTime::<Utc>::from_timestamp_nanos(nanos);
            let timestamp = StorylineTimestamp::from_utc(instant).unwrap();
            let reparsed = StorylineTimestamp::from_rfc3339(&timestamp.canonical_rfc3339()).unwrap();
            prop_assert_eq!(reparsed.instant(), instant);
            prop_assert_eq!(reparsed.timestamp_nanos(), nanos);
        }
    }
}
