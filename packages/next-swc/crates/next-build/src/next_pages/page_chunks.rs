use anyhow::Result;
use next_core::{
    env::env_for_js,
    mode::NextMode,
    next_client::{
        get_client_compile_time_info, get_client_module_options_context,
        get_client_resolve_options_context, get_client_runtime_entries, ClientContextType,
        RuntimeEntries, RuntimeEntry,
    },
    next_client_chunks::NextClientChunksTransition,
    next_config::NextConfig,
    next_server::{
        get_server_compile_time_info, get_server_module_options_context,
        get_server_resolve_options_context, ServerContextType,
    },
    pages_structure::{
        OptionPagesStructure, PagesDirectoryStructure, PagesStructure, PagesStructureItem,
    },
    pathname_for_path,
    turbopack::core::asset::Assets,
    PathType,
};
use turbo_tasks::Vc;
use turbopack_binding::{
    turbo::{tasks::Value, tasks_env::ProcessEnv, tasks_fs::FileSystemPath},
    turbopack::{
        core::{
            asset::Asset,
            context::AssetContext,
            environment::ServerAddr,
            reference_type::{EntryReferenceSubType, ReferenceType},
            source_asset::SourceAsset,
        },
        env::ProcessEnvAsset,
        node::execution_context::ExecutionContext,
        turbopack::{transition::TransitionsByName, ModuleAssetContext},
    },
};

use super::{client_context::PagesBuildClientContext, node_context::PagesBuildNodeContext};

#[turbo_tasks::value(transparent)]
pub struct PageChunks(Vec<Vc<PageChunk>>);

#[turbo_tasks::value_impl]
impl PageChunks {
    #[turbo_tasks::function]
    pub fn empty() -> Vc<Self> {
        PageChunks(vec![]).cell()
    }
}

/// Returns a list of page chunks.
#[turbo_tasks::function]
pub async fn get_page_chunks(
    pages_structure: Vc<OptionPagesStructure>,
    project_root: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    node_root: Vc<FileSystemPath>,
    client_root: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    browserslist_query: String,
    next_config: Vc<NextConfig>,
    node_addr: Vc<ServerAddr>,
) -> Result<Vc<PageChunks>> {
    let Some(pages_structure) = *pages_structure.await? else {
        return Ok(PageChunks::empty());
    };
    let pages_dir = pages_structure.project_path().resolve().await?;

    let mode = NextMode::Build;

    let client_ty = Value::new(ClientContextType::Pages { pages_dir });
    let node_ty = Value::new(ServerContextType::Pages { pages_dir });

    let client_compile_time_info = get_client_compile_time_info(mode, browserslist_query);

    let transitions = Vc::cell(
        [(
            // This is necessary for the next dynamic transform to work.
            // TODO(alexkirsz) Should accept client chunking context? But how do we get this?
            "next-client-chunks".to_string(),
            Vc::upcast(NextClientChunksTransition::new(
                project_root,
                execution_context,
                client_ty,
                mode,
                client_root,
                client_compile_time_info,
                next_config,
            )),
        )]
        .into_iter()
        .collect(),
    );

    let client_module_options_context = get_client_module_options_context(
        project_root,
        execution_context,
        client_compile_time_info.environment(),
        client_ty,
        mode,
        next_config,
    );
    let client_resolve_options_context = get_client_resolve_options_context(
        project_root,
        client_ty,
        mode,
        next_config,
        execution_context,
    );
    let client_asset_context: Vc<Box<dyn AssetContext>> = Vc::upcast(ModuleAssetContext::new(
        transitions,
        client_compile_time_info,
        client_module_options_context,
        client_resolve_options_context,
    ));

    let node_compile_time_info = get_server_compile_time_info(node_ty, mode, env, node_addr);
    let node_resolve_options_context = get_server_resolve_options_context(
        project_root,
        node_ty,
        mode,
        next_config,
        execution_context,
    );
    let node_module_options_context = get_server_module_options_context(
        project_root,
        execution_context,
        node_ty,
        mode,
        next_config,
    );

    let node_asset_context = Vc::upcast(ModuleAssetContext::new(
        transitions,
        node_compile_time_info,
        node_module_options_context,
        node_resolve_options_context,
    ));

    let node_runtime_entries = get_node_runtime_entries(project_root, env, next_config);

    let client_runtime_entries = get_client_runtime_entries(
        project_root,
        env,
        client_ty,
        mode,
        next_config,
        execution_context,
    );
    let client_runtime_entries = client_runtime_entries.resolve_entries(client_asset_context);

    let node_build_context = PagesBuildNodeContext::new(
        project_root,
        node_root,
        node_asset_context,
        node_runtime_entries,
    );
    let client_build_context = PagesBuildClientContext::new(
        project_root,
        client_root,
        client_asset_context,
        client_runtime_entries,
    );

    Ok(get_page_chunks_for_root_directory(
        node_build_context,
        client_build_context,
        pages_structure,
    ))
}

#[turbo_tasks::function]
async fn get_page_chunks_for_root_directory(
    node_build_context: Vc<PagesBuildNodeContext>,
    client_build_context: Vc<PagesBuildClientContext>,
    pages_structure: Vc<PagesStructure>,
) -> Result<Vc<PageChunks>> {
    let PagesStructure {
        app,
        document,
        error,
        api,
        pages,
    } = *pages_structure.await?;
    let mut chunks = vec![];

    let next_router_root = pages.next_router_path();

    // This only makes sense on both the client and the server, but they should map
    // to different assets (server can be an external module).
    let app = app.await?;
    chunks.push(get_page_chunk_for_file(
        node_build_context,
        client_build_context,
        Vc::upcast(SourceAsset::new(app.project_path)),
        next_router_root,
        app.next_router_path,
    ));

    // This only makes sense on the server.
    let document = document.await?;
    chunks.push(get_page_chunk_for_file(
        node_build_context,
        client_build_context,
        Vc::upcast(SourceAsset::new(document.project_path)),
        next_router_root,
        document.next_router_path,
    ));

    // This only makes sense on both the client and the server, but they should map
    // to different assets (server can be an external module).
    let error = error.await?;
    chunks.push(get_page_chunk_for_file(
        node_build_context,
        client_build_context,
        Vc::upcast(SourceAsset::new(error.project_path)),
        next_router_root,
        error.next_router_path,
    ));

    if let Some(api) = api {
        chunks.extend(
            get_page_chunks_for_directory(
                node_build_context,
                client_build_context,
                api,
                next_router_root,
            )
            .await?
            .iter()
            .copied(),
        );
    }

    chunks.extend(
        get_page_chunks_for_directory(
            node_build_context,
            client_build_context,
            pages,
            next_router_root,
        )
        .await?
        .iter()
        .copied(),
    );

    Ok(Vc::cell(chunks))
}

#[turbo_tasks::function]
async fn get_page_chunks_for_directory(
    node_build_context: Vc<PagesBuildNodeContext>,
    client_build_context: Vc<PagesBuildClientContext>,
    pages_structure: Vc<PagesDirectoryStructure>,
    next_router_root: Vc<FileSystemPath>,
) -> Result<Vc<PageChunks>> {
    let PagesDirectoryStructure {
        ref items,
        ref children,
        ..
    } = *pages_structure.await?;
    let mut chunks = vec![];

    for item in items.iter() {
        let PagesStructureItem {
            project_path,
            next_router_path,
            specificity: _,
        } = *item.await?;
        chunks.push(get_page_chunk_for_file(
            node_build_context,
            client_build_context,
            Vc::upcast(SourceAsset::new(project_path)),
            next_router_root,
            next_router_path,
        ));
    }

    for child in children.iter() {
        chunks.extend(
            // TODO(alexkirsz) This should be a tree structure instead of a flattened list.
            get_page_chunks_for_directory(
                node_build_context,
                client_build_context,
                *child,
                next_router_root,
            )
            .await?
            .iter()
            .copied(),
        )
    }

    Ok(Vc::cell(chunks))
}

/// A page chunk corresponding to some route.
#[turbo_tasks::value]
pub struct PageChunk {
    /// The pathname of the page.
    pub pathname: Vc<String>,
    /// The Node.js chunk.
    pub node_chunk: Vc<Box<dyn Asset>>,
    /// The client chunks.
    pub client_chunks: Vc<Assets>,
}

#[turbo_tasks::function]
async fn get_page_chunk_for_file(
    node_build_context: Vc<PagesBuildNodeContext>,
    client_build_context: Vc<PagesBuildClientContext>,
    page_asset: Vc<Box<dyn Asset>>,
    next_router_root: Vc<FileSystemPath>,
    next_router_path: Vc<FileSystemPath>,
) -> Result<Vc<PageChunk>> {
    let reference_type = Value::new(ReferenceType::Entry(EntryReferenceSubType::Page));

    let pathname = pathname_for_path(next_router_root, next_router_path, PathType::Page);

    Ok(PageChunk {
        pathname,
        node_chunk: node_build_context.node_chunk(page_asset, reference_type.clone()),
        client_chunks: client_build_context.client_chunk(page_asset, pathname, reference_type),
    }
    .cell())
}

#[turbo_tasks::function]
async fn pathname_from_path(next_router_path: Vc<FileSystemPath>) -> Result<Vc<String>> {
    let pathname = next_router_path.await?;
    Ok(Vc::cell(pathname.path.clone()))
}

#[turbo_tasks::function]
fn get_node_runtime_entries(
    project_root: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    next_config: Vc<NextConfig>,
) -> Vc<RuntimeEntries> {
    let node_runtime_entries = vec![RuntimeEntry::Source(
        ProcessEnvAsset::new(project_root, env_for_js(env, false, next_config)).into(),
    )
    .cell()];

    Vc::cell(node_runtime_entries)
}
