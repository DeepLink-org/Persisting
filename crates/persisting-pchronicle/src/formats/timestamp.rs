use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
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
            Value::String(value) => parse_timestamp_string(value).ok_or_else(|| {
                InputIssue::invalid(
                    "timestamp string must be RFC3339 or a recognized date/time / Unix form",
                )
            })?,
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

    /// Best-effort parse for optional timestamps: try alternate forms, else `None`.
    pub fn from_json_lenient(source: Value) -> Option<Self> {
        Self::from_json(source).ok()
    }

    pub fn from_rfc3339(value: &str) -> InputResult<Self> {
        Self::from_json(Value::String(value.to_string()))
    }

    /// Soft string parse used by converters: unrecognized values become `None`.
    pub fn from_rfc3339_lenient(value: &str) -> Option<Self> {
        Self::from_json_lenient(Value::String(value.to_string()))
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

/// Deserialize `Option<StorylineTimestamp>`: null/missing → None; unparseable → None.
pub fn deserialize_optional_timestamp<'de, D>(
    deserializer: D,
) -> Result<Option<StorylineTimestamp>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(value) => StorylineTimestamp::from_json_lenient(value),
    })
}

/// Parse common timestamp string forms into UTC.
///
/// Order: RFC3339 → RFC3339-ish with assumed UTC → Naive local forms as UTC →
/// offset forms → Unix seconds/millis/micros encoded as decimal strings.
fn parse_timestamp_string(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Some(dt.with_timezone(&Utc));
    }

    // Space separator / missing `Z`: normalize then retry RFC3339.
    if let Some(normalized) = normalize_toward_rfc3339(value)
        && let Ok(dt) = DateTime::parse_from_rfc3339(&normalized)
    {
        return Some(dt.with_timezone(&Utc));
    }

    const WITH_OFFSET: &[&str] = &[
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y/%m/%d %H:%M:%S%.f%:z",
        "%Y/%m/%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%dT%H:%M:%S%z",
    ];
    for fmt in WITH_OFFSET {
        if let Ok(dt) = DateTime::parse_from_str(value, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }

    const NAIVE_UTC: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S%.f",
        "%Y/%m/%d %H:%M:%S",
        "%Y/%m/%dT%H:%M:%S%.f",
        "%Y/%m/%dT%H:%M:%S",
    ];
    for fmt in NAIVE_UTC {
        if let Ok(naive) = NaiveDateTime::parse_from_str(value, fmt) {
            return Some(naive.and_utc());
        }
    }

    parse_unix_string(value)
}

fn normalize_toward_rfc3339(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.len() < 11 {
        return None;
    }
    // `2026-08-20 12:00:00` / `2026/08/20 12:00:00` → `T` + optional `Z`
    let mut candidate = trimmed.replace('/', "-");
    if candidate.as_bytes().get(10) == Some(&b' ') {
        candidate.replace_range(10..11, "T");
    }
    let tail = &candidate[10..];
    let has_zone = candidate.ends_with('Z')
        || candidate.ends_with('z')
        || tail.contains('+')
        || tail.rfind('-').is_some_and(|idx| idx > 0);
    if !has_zone {
        candidate.push('Z');
    }
    if candidate == trimmed {
        None
    } else {
        Some(candidate)
    }
}

fn parse_unix_string(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(n) = value.parse::<i64>() {
        return unix_i64_to_utc(n);
    }
    // `"1710000000.25"` → seconds with fraction
    if value.contains('.')
        && let Ok(nanos) = decimal_seconds_to_nanos(value)
    {
        return Some(DateTime::<Utc>::from_timestamp_nanos(nanos));
    }
    None
}

fn unix_i64_to_utc(n: i64) -> Option<DateTime<Utc>> {
    let abs = n.unsigned_abs();
    // Heuristic by magnitude (absolute value):
    //   < 1e11  → seconds  (year ~5138)
    //   < 1e14  → millis
    //   else    → micros
    if abs < 100_000_000_000 {
        DateTime::from_timestamp(n, 0)
    } else if abs < 100_000_000_000_000 {
        DateTime::from_timestamp_millis(n)
    } else {
        DateTime::from_timestamp_micros(n)
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

    #[test]
    fn parses_common_non_rfc3339_strings() {
        let cases = [
            "2026-08-20 12:00:00",
            "2026/08/20 12:00:00",
            "2026-08-20T12:00:00",
            "2026-08-20 12:00:00.123456",
            "2026/08/20T12:00:00.5",
        ];
        for raw in cases {
            let ts = StorylineTimestamp::from_rfc3339(raw)
                .unwrap_or_else(|error| panic!("expected parse for {raw}: {error}"));
            assert_eq!(ts.source_string(), Some(raw));
            assert!(ts.instant().timestamp() > 0, "{raw}");
        }
    }

    #[test]
    fn parses_unix_seconds_as_string() {
        let ts = StorylineTimestamp::from_rfc3339("1710000000").unwrap();
        assert_eq!(ts.instant().timestamp(), 1710000000);
    }

    #[test]
    fn lenient_returns_none_for_garbage() {
        assert!(StorylineTimestamp::from_rfc3339_lenient("not-a-time").is_none());
        assert!(StorylineTimestamp::from_rfc3339_lenient("").is_none());
    }

    #[cfg(feature = "proptest")]
    mod proptests {
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

            #[test]
            fn decimal_encoder_never_emits_excess_fractional_precision(nanos in any::<i64>()) {
                let encoded = nanos_as_decimal(nanos);
                prop_assert!(encoded.split_once('.').map_or(true, |(_, fraction)| fraction.len() <= 9));
            }
        }
    }
}
