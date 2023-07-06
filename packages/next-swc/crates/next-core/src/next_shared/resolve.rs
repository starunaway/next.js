use std::collections::{HashMap, HashSet};

use anyhow::Result;
use lazy_static::lazy_static;
use turbo_tasks::primitives::StringVc;
use turbo_tasks_fs::glob::GlobVc;
use turbopack_binding::{
    turbo::tasks_fs::FileSystemPathVc,
    turbopack::core::{
        issue::unsupported_module::UnsupportedModuleIssue,
        resolve::{
            parse::{Request, RequestVc},
            pattern::Pattern,
            plugin::{ResolvePlugin, ResolvePluginConditionVc, ResolvePluginVc},
            ResolveResultOptionVc,
        },
    },
};

use crate::next_telemetry::ModuleFeatureTelemetry;

lazy_static! {
    static ref UNSUPPORTED_PACKAGES: HashSet<&'static str> = ["@vercel/og"].into();
    static ref UNSUPPORTED_PACKAGE_PATHS: HashSet<(&'static str, &'static str)> = [].into();
    // Set of the features we want to track, following existing references in webpack/plugins/telemetry-plugin.
    static ref FEATURE_MODULES: HashMap<&'static str, Vec<&'static str>> = HashMap::from([
        (
            "next",
            vec![
                "/image",
                "future/image",
                "legacy/image",
                "/script",
                "/dynamic",
                "/font/google",
                "/font/local"
            ]
        ),
        ("@next", vec!["/font/google", "/font/local"])
    ])
    .into();
}

#[turbo_tasks::value]
pub(crate) struct UnsupportedModulesResolvePlugin {
    root: FileSystemPathVc,
}

#[turbo_tasks::value_impl]
impl UnsupportedModulesResolvePluginVc {
    #[turbo_tasks::function]
    pub fn new(root: FileSystemPathVc) -> Self {
        UnsupportedModulesResolvePlugin { root }.cell()
    }
}

#[turbo_tasks::value_impl]
impl ResolvePlugin for UnsupportedModulesResolvePlugin {
    #[turbo_tasks::function]
    fn after_resolve_condition(&self) -> ResolvePluginConditionVc {
        ResolvePluginConditionVc::new(self.root.root(), GlobVc::new("**"))
    }

    #[turbo_tasks::function]
    async fn after_resolve(
        &self,
        _fs_path: FileSystemPathVc,
        context: FileSystemPathVc,
        request: RequestVc,
    ) -> Result<ResolveResultOptionVc> {
        if let Request::Module {
            module,
            path,
            query: _,
        } = &*request.await?
        {
            // Warn if the package is known not to be supported by Turbopack at the moment.
            if UNSUPPORTED_PACKAGES.contains(module.as_str()) {
                UnsupportedModuleIssue {
                    context,
                    package: module.into(),
                    package_path: None,
                }
                .cell()
                .as_issue()
                .emit();
            }

            if let Pattern::Constant(path) = path {
                if UNSUPPORTED_PACKAGE_PATHS.contains(&(module, path)) {
                    UnsupportedModuleIssue {
                        context,
                        package: module.into(),
                        package_path: Some(path.to_owned()),
                    }
                    .cell()
                    .as_issue()
                    .emit();
                }
            }
        }

        Ok(ResolveResultOptionVc::none())
    }
}

/// A resolver plugin trackes the usage of certain import paths, emit a
/// telemetry event if there is a match.
#[turbo_tasks::value]
pub(crate) struct ModuleFeatureReportResolvePlugin {
    root: FileSystemPathVc,
    event_name: StringVc,
}

#[turbo_tasks::value_impl]
impl ModuleFeatureReportResolvePluginVc {
    #[turbo_tasks::function]
    pub fn new(root: FileSystemPathVc, event_name: StringVc) -> Self {
        ModuleFeatureReportResolvePlugin { root, event_name }.cell()
    }
}

#[turbo_tasks::value_impl]
impl ResolvePlugin for ModuleFeatureReportResolvePlugin {
    #[turbo_tasks::function]
    fn after_resolve_condition(&self) -> ResolvePluginConditionVc {
        ResolvePluginConditionVc::new(self.root.root(), GlobVc::new("**"))
    }

    #[turbo_tasks::function]
    async fn after_resolve(
        &self,
        _fs_path: FileSystemPathVc,
        _context: FileSystemPathVc,
        request: RequestVc,
    ) -> Result<ResolveResultOptionVc> {
        if let Request::Module {
            module,
            path,
            query: _,
        } = &*request.await?
        {
            let feature_module = FEATURE_MODULES.get(module.as_str());
            if let Some(feature_module) = feature_module {
                let sub_path = feature_module
                    .iter()
                    .find(|sub_path| path.is_match(sub_path));

                if let Some(sub_path) = sub_path {
                    ModuleFeatureTelemetry {
                        event_name: self.event_name.await?.to_string(),
                        feature_name: format!("{}{}", module, sub_path),
                        invocation_count: 1,
                    }
                    .cell()
                    .as_next_telemetry()
                    .emit();
                }
            }
        }

        Ok(ResolveResultOptionVc::none())
    }
}
