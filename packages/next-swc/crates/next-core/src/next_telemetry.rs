use anyhow::Result;
use turbo_tasks::{emit, primitives::StringVc, ValueToString, ValueToStringVc};

#[turbo_tasks::value_trait]
pub trait NextTelemetry {
    fn event_name(&self) -> StringVc;
}

impl NextTelemetryVc {
    pub fn emit(self) {
        emit(self);
    }
}

/// A struct represent telemetry event for feature usage,
/// referred as `importing` a certain module.
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
