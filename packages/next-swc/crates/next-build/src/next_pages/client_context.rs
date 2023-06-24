use anyhow::{bail, Result};
use next_core::{
    create_page_loader_entry_asset,
    turbopack::core::{asset::Assets, chunk::EvaluatableAssets},
};
use turbo_tasks::Vc;
use turbopack_binding::{
    turbo::{tasks::Value, tasks_fs::FileSystemPath},
    turbopack::{
        core::{
            asset::Asset,
            chunk::{ChunkableAsset, ChunkingContext},
            context::AssetContext,
            reference_type::ReferenceType,
        },
        dev::DevChunkingContext,
        ecmascript::EcmascriptModuleAsset,
    },
};

#[turbo_tasks::value]
pub(crate) struct PagesBuildClientContext {
    project_root: Vc<FileSystemPath>,
    client_root: Vc<FileSystemPath>,
    client_asset_context: Vc<Box<dyn AssetContext>>,
    client_runtime_entries: Vc<EvaluatableAssets>,
}

#[turbo_tasks::value_impl]
impl PagesBuildClientContext {
    #[turbo_tasks::function]
    pub fn new(
        project_root: Vc<FileSystemPath>,
        client_root: Vc<FileSystemPath>,
        client_asset_context: Vc<Box<dyn AssetContext>>,
        client_runtime_entries: Vc<EvaluatableAssets>,
    ) -> Vc<PagesBuildClientContext> {
        PagesBuildClientContext {
            project_root,
            client_root,
            client_asset_context,
            client_runtime_entries,
        }
        .cell()
    }

    #[turbo_tasks::function]
    async fn client_chunking_context(self: Vc<Self>) -> Result<Vc<Box<dyn ChunkingContext>>> {
        let this = self.await?;

        Ok(DevChunkingContext::builder(
            this.project_root,
            this.client_root,
            this.client_root.join("static/chunks"),
            this.client_root.join("static/media"),
            this.client_asset_context.compile_time_info().environment(),
        )
        .build())
    }

    #[turbo_tasks::function]
    pub async fn client_chunk(
        self: Vc<Self>,
        asset: Vc<Box<dyn Asset>>,
        pathname: Vc<String>,
        reference_type: Value<ReferenceType>,
    ) -> Result<Vc<Assets>> {
        let this = self.await?;

        let client_asset_page = this.client_asset_context.process(asset, reference_type);
        let client_asset_page =
            create_page_loader_entry_asset(this.client_asset_context, client_asset_page, pathname);

        let Some(client_module_asset) = Vc::try_resolve_downcast_type::<EcmascriptModuleAsset>(client_asset_page).await? else {
            bail!("Expected an EcmaScript module asset");
        };

        let client_chunking_context = self.client_chunking_context();

        Ok(client_chunking_context.evaluated_chunk_group(
            client_module_asset.as_root_chunk(client_chunking_context),
            this.client_runtime_entries
                .with_entry(client_module_asset.into()),
        ))
    }
}
