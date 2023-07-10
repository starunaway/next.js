use anyhow::Result;
use turbo_tasks::{emit, primitives::StringVc, ValueToString, ValueToStringVc};
use turbopack_binding::features::auto_hash_map;

/// A list of issues captured with
/// [`NextTelemetryVc::peek_telemetries_with_path`] and
#[derive(Debug)]
#[turbo_tasks::value]
pub struct CapturedTelemetry {
    pub telemetries: auto_hash_map::AutoSet<NextTelemetryVc>,
}

#[turbo_tasks::value_trait]
pub trait NextTelemetry {
    // [TODO]: this is likely to change to match to parity to the existing
    // telemetry payload.
    fn event_name(&self) -> StringVc;
}

impl NextTelemetryVc {
    pub fn emit(self) {
        emit(self);
    }

    pub async fn peek_telemetries_with_path<T: turbo_tasks::CollectiblesSource + Copy>(
        source: T,
    ) -> Result<CapturedTelemetryVc> {
        Ok(CapturedTelemetryVc::cell(CapturedTelemetry {
            telemetries: source.peek_collectibles().strongly_consistent().await?,
        }))
    }
}

#[turbo_tasks::value_trait]
pub trait TelemetryReporter {
    fn report(
        &self,
        telemetries: turbo_tasks::TransientInstance<turbo_tasks::ReadRef<CapturedTelemetry>>,
        source: turbo_tasks::TransientValue<turbo_tasks::RawVc>,
    ) -> turbo_tasks::primitives::BoolVc;
}

/// A struct represent telemetry event for the feature usage,
/// referred as `importing` a certain module. (i.e importing @next/image)
#[turbo_tasks::value(shared)]
pub struct ModuleFeatureTelemetry {
    pub event_name: String,
    pub feature_name: String,
    pub invocation_count: usize,
}

impl ModuleFeatureTelemetryVc {
    pub fn new(name: String, feature: String, invocation_count: usize) -> Self {
        Self::cell(ModuleFeatureTelemetry {
            event_name: name,
            feature_name: feature,
            invocation_count,
        })
    }
}

#[turbo_tasks::value_impl]
impl ValueToString for ModuleFeatureTelemetry {
    #[turbo_tasks::function]
    fn to_string(&self) -> StringVc {
        StringVc::cell(format!("{},{}", self.event_name, self.feature_name))
    }
}

#[turbo_tasks::value_impl]
impl NextTelemetry for ModuleFeatureTelemetry {
    #[turbo_tasks::function]
    async fn event_name(&self) -> Result<StringVc> {
        Ok(StringVc::cell(self.event_name.clone()))
    }
}
