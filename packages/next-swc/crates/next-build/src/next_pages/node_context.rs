use anyhow::{bail, Result};
use next_core::{next_client::RuntimeEntries, turbopack::core::chunk::EvaluatableAssets};
use turbo_tasks::Vc;
use turbopack_binding::{
    turbo::{tasks::Value, tasks_fs::FileSystemPath},
    turbopack::{
        build::BuildChunkingContext,
        core::{
            asset::Asset,
            context::AssetContext,
            reference_type::{EntryReferenceSubType, ReferenceType},
            resolve::{parse::Request, pattern::QueryMap},
        },
        ecmascript::EcmascriptModuleAsset,
    },
};

#[turbo_tasks::value]
pub(crate) struct PagesBuildNodeContext {
    project_root: Vc<FileSystemPath>,
    node_root: Vc<FileSystemPath>,
    node_asset_context: Vc<Box<dyn AssetContext>>,
    node_runtime_entries: Vc<EvaluatableAssets>,
}

#[turbo_tasks::value_impl]
impl PagesBuildNodeContext {
    #[turbo_tasks::function]
    pub fn new(
        project_root: Vc<FileSystemPath>,
        node_root: Vc<FileSystemPath>,
        node_asset_context: Vc<Box<dyn AssetContext>>,
        node_runtime_entries: Vc<RuntimeEntries>,
    ) -> Vc<PagesBuildNodeContext> {
        PagesBuildNodeContext {
            project_root,
            node_root,
            node_asset_context,
            node_runtime_entries: node_runtime_entries.resolve_entries(node_asset_context),
        }
        .cell()
    }

    #[turbo_tasks::function]
    pub async fn resolve_module(
        self: Vc<Self>,
        origin: Vc<FileSystemPath>,
        package: String,
        path: String,
    ) -> Result<Vc<Box<dyn Asset>>> {
        let this = self.await?;
        let Some(asset) = this
            .node_asset_context
            .resolve_asset(
                origin,
                Request::module(package.clone(), Value::new(path.clone().into()), QueryMap::none()),
                this.node_asset_context.resolve_options(origin, Value::new(ReferenceType::Entry(EntryReferenceSubType::Page))),
                Value::new(ReferenceType::Entry(EntryReferenceSubType::Page))
            )
            .primary_assets()
            .await?
            .first()
            .copied()
        else {
            bail!("module {}/{} not found in {}", package, path, origin.await?);
        };
        Ok(asset)
    }

    #[turbo_tasks::function]
    async fn node_chunking_context(self: Vc<Self>) -> Result<Vc<BuildChunkingContext>> {
        let this = self.await?;

        Ok(BuildChunkingContext::builder(
            this.project_root,
            this.node_root,
            this.node_root.join("server/pages"),
            this.node_root.join("server/assets"),
            this.node_asset_context.compile_time_info().environment(),
        )
        .build())
    }

    #[turbo_tasks::function]
    pub async fn node_chunk(
        self: Vc<Self>,
        asset: Vc<Box<dyn Asset>>,
        reference_type: Value<ReferenceType>,
    ) -> Result<Vc<Box<dyn Asset>>> {
        let this = self.await?;

        let node_asset_page = this.node_asset_context.process(asset, reference_type);

        let Some(node_module_asset) = Vc::try_resolve_downcast_type::<EcmascriptModuleAsset>(node_asset_page).await? else {
            bail!("Expected an EcmaScript module asset");
        };

        let chunking_context = self.node_chunking_context();
        Ok(chunking_context.generate_exported_chunk(node_module_asset, this.node_runtime_entries))
    }
}
