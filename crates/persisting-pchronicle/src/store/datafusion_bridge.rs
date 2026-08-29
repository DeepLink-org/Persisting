use datafusion::error::DataFusionError;

#[derive(Debug)]
struct ExternalFailure(anyhow::Error);

impl std::fmt::Display for ExternalFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for ExternalFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

pub(super) fn into_datafusion(error: anyhow::Error) -> DataFusionError {
    DataFusionError::External(Box::new(ExternalFailure(error)))
}

pub(super) fn from_datafusion(operation: &'static str, error: DataFusionError) -> anyhow::Error {
    match error {
        DataFusionError::External(source) => match source.downcast::<ExternalFailure>() {
            Ok(source) => source.0.context(operation),
            Err(source) => anyhow::Error::new(DataFusionError::External(source)).context(operation),
        },
        source => anyhow::Error::new(source).context(operation),
    }
}

#[cfg(test)]
mod tests {
    use super::{from_datafusion, into_datafusion};

    #[test]
    fn external_roundtrip_preserves_root_source() {
        let error =
            anyhow::Error::new(std::io::Error::other("disk sentinel")).context("read source");

        let recovered = from_datafusion("execute query", into_datafusion(error));
        let rendered = format!("{recovered:#}");

        assert!(rendered.contains("execute query"));
        assert!(rendered.contains("read source"));
        assert!(rendered.contains("disk sentinel"));
    }

    #[test]
    fn native_datafusion_failure_remains_a_source() {
        let recovered = from_datafusion(
            "plan query",
            datafusion::error::DataFusionError::Plan("bad plan".into()),
        );

        assert!(format!("{recovered:#}").contains("bad plan"));
    }

    #[cfg(feature = "proptest")]
    mod proptests {
        use datafusion::error::DataFusionError;
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn external_errors_preserve_operation_and_message(
                operation in proptest::string::string_regex("[A-Za-z0-9 _-]{1,32}").unwrap(),
                message in proptest::string::string_regex("[A-Za-z0-9 _-]{1,64}").unwrap(),
            ) {
                let error = anyhow::anyhow!("{message}");
                let recovered = from_datafusion(Box::leak(operation.clone().into_boxed_str()), into_datafusion(error));
                let rendered = format!("{recovered:#}");
                prop_assert!(rendered.contains(&operation));
                prop_assert!(rendered.contains(&message));
            }

            #[test]
            fn native_plan_errors_keep_their_text(
                message in proptest::string::string_regex("[A-Za-z0-9 _-]{1,64}").unwrap(),
            ) {
                let recovered = from_datafusion("plan", DataFusionError::Plan(message.clone()));
                prop_assert!(format!("{recovered:#}").contains(&message), "recovered={:?}", recovered);
            }
        }
    }
}
