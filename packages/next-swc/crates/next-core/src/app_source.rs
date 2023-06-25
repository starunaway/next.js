use std::{collections::HashMap, io::Write as _, iter::once};

use anyhow::{bail, Result};
use async_recursion::async_recursion;
use indexmap::{indexmap, IndexMap};
use indoc::indoc;
use serde_json::Value as JsonValue;
use turbo_tasks::{TryJoinIterExt, ValueToString, Vc};
use turbopack_binding::{
    turbo::{
        tasks::Value,
        tasks_env::{CustomProcessEnv, EnvMap, ProcessEnv},
        tasks_fs::{rope::RopeBuilder, File, FileSystemPath},
    },
    turbopack::{
        core::{
            asset::{Asset, AssetContent, Assets},
            chunk::{EvaluatableAsset, EvaluatableAssetExt},
            compile_time_info::CompileTimeInfo,
            context::AssetContext,
            environment::{EnvironmentIntention, ServerAddr},
            issue::{Issue, IssueExt, IssueSeverity},
            reference_type::{
                EcmaScriptModulesReferenceSubType, EntryReferenceSubType, InnerAssets,
                ReferenceType,
            },
            source_asset::SourceAsset,
            virtual_asset::VirtualAsset,
        },
        dev::DevChunkingContext,
        dev_server::{
            html::DevHtmlAsset,
            source::{
                asset_graph::AssetGraphContentSource,
                combined::CombinedContentSource,
                specificity::{Specificity, SpecificityElementType},
                ContentSource, ContentSourceData, ContentSourceExt, NoContentSource,
            },
        },
        ecmascript::{
            magic_identifier,
            text::TextContentSourceAsset,
            utils::{FormatIter, StringifyJs},
        },
        env::ProcessEnvAsset,
        node::{
            debug::should_debug,
            execution_context::ExecutionContext,
            render::{
                node_api_source::create_node_api_source,
                rendered_source::create_node_rendered_source,
            },
            NodeEntry, NodeRenderingEntry,
        },
        r#static::{fixed::FixedStaticAsset, StaticModuleAsset},
        turbopack::{transition::Transition, ModuleAssetContext},
    },
};

use crate::{
    app_render::next_server_component_transition::NextServerComponentTransition,
    app_segment_config::{parse_segment_config_from_loader_tree, parse_segment_config_from_source},
    app_structure::{
        get_entrypoints, get_global_metadata, Components, Entrypoint, GlobalMetadata, LoaderTree,
        Metadata, MetadataItem, MetadataWithAltItem, OptionAppDir,
    },
    bootstrap::{route_bootstrap, BootstrapConfig},
    embed_js::{next_asset, next_js_file_path},
    env::env_for_js,
    fallback::get_fallback_page,
    mode::NextMode,
    next_client::{
        context::{
            get_client_assets_path, get_client_chunking_context, get_client_compile_time_info,
            get_client_module_options_context, get_client_resolve_options_context,
            get_client_runtime_entries, ClientContextType,
        },
        transition::NextClientTransition,
    },
    next_client_chunks::client_chunks_transition::NextClientChunksTransition,
    next_client_component::{
        server_to_client_transition::NextServerToClientTransition,
        ssr_client_module_transition::NextSSRClientModuleTransition,
    },
    next_config::NextConfig,
    next_edge::{
        context::{get_edge_compile_time_info, get_edge_resolve_options_context},
        page_transition::NextEdgePageTransition,
        route_transition::NextEdgeRouteTransition,
    },
    next_image::module::{BlurPlaceholderMode, StructuredImageModuleType},
    next_route_matcher::{NextFallbackMatcher, NextParamsMatcher},
    next_server::context::{
        get_server_compile_time_info, get_server_module_options_context,
        get_server_resolve_options_context, ServerContextType,
    },
    util::{render_data, NextRuntime},
};

#[turbo_tasks::function]
fn pathname_to_specificity(pathname: String) -> Vc<Specificity> {
    let mut current = Specificity::new();
    let mut position = 0;
    for segment in pathname.split('/') {
        if segment.starts_with('(') && segment.ends_with(')') || segment.starts_with('@') {
            // ignore
        } else if segment.starts_with("[[...") && segment.ends_with("]]")
            || segment.starts_with("[...") && segment.ends_with(']')
        {
            // optional catch all segment
            current.add(position - 1, SpecificityElementType::CatchAll);
            position += 1;
        } else if segment.starts_with("[[") || segment.ends_with("]]") {
            // optional segment
            position += 1;
        } else if segment.starts_with('[') || segment.ends_with(']') {
            current.add(position - 1, SpecificityElementType::DynamicSegment);
            position += 1;
        } else {
            // normal segment
            position += 1;
        }
    }
    Specificity::cell(current)
}

#[turbo_tasks::function]
async fn next_client_transition(
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    server_root: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    client_compile_time_info: Vc<CompileTimeInfo>,
    next_config: Vc<NextConfig>,
) -> Result<Vc<Box<dyn Transition>>> {
    let ty = Value::new(ClientContextType::App { app_dir });
    let mode = NextMode::Development;
    let client_chunking_context = get_client_chunking_context(
        project_path,
        server_root,
        client_compile_time_info.environment(),
        ty,
    );
    let client_module_options_context = get_client_module_options_context(
        project_path,
        execution_context,
        client_compile_time_info.environment(),
        ty,
        mode,
        next_config,
    );
    let client_runtime_entries =
        get_client_runtime_entries(project_path, env, ty, mode, next_config, execution_context);
    let client_resolve_options_context =
        get_client_resolve_options_context(project_path, ty, mode, next_config, execution_context);

    Ok(Vc::upcast(
        NextClientTransition {
            is_app: true,
            client_chunking_context,
            client_module_options_context,
            client_resolve_options_context,
            client_compile_time_info,
            runtime_entries: client_runtime_entries,
        }
        .cell(),
    ))
}

#[turbo_tasks::function]
fn next_ssr_client_module_transition(
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    app_dir: Vc<FileSystemPath>,
    process_env: Vc<Box<dyn ProcessEnv>>,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
) -> Vc<Box<dyn Transition>> {
    let ty = Value::new(ServerContextType::AppSSR { app_dir });
    let mode = NextMode::Development;
    Vc::upcast(
        NextSSRClientModuleTransition {
            ssr_module_options_context: get_server_module_options_context(
                project_path,
                execution_context,
                ty,
                mode,
                next_config,
            ),
            ssr_resolve_options_context: get_server_resolve_options_context(
                project_path,
                ty,
                mode,
                next_config,
                execution_context,
            ),
            ssr_environment: get_server_compile_time_info(ty, mode, process_env, server_addr),
        }
        .cell(),
    )
}

#[turbo_tasks::function]
fn next_server_component_transition(
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    app_dir: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    process_env: Vc<Box<dyn ProcessEnv>>,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
) -> Vc<Box<dyn Transition>> {
    let ty = Value::new(ServerContextType::AppRSC { app_dir });
    let mode = NextMode::Development;
    let rsc_compile_time_info = get_server_compile_time_info(ty, mode, process_env, server_addr);
    let rsc_resolve_options_context =
        get_server_resolve_options_context(project_path, ty, mode, next_config, execution_context);
    let rsc_module_options_context =
        get_server_module_options_context(project_path, execution_context, ty, mode, next_config);

    Vc::upcast(
        NextServerComponentTransition {
            rsc_compile_time_info,
            rsc_module_options_context,
            rsc_resolve_options_context,
            server_root,
        }
        .cell(),
    )
}

#[turbo_tasks::function]
fn next_edge_server_component_transition(
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    app_dir: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
) -> Vc<Box<dyn Transition>> {
    let ty = Value::new(ServerContextType::AppRSC { app_dir });
    let mode = NextMode::Development;
    let rsc_compile_time_info = get_edge_compile_time_info(
        project_path,
        server_addr,
        Value::new(EnvironmentIntention::ServerRendering),
    );
    let rsc_resolve_options_context =
        get_edge_resolve_options_context(project_path, ty, next_config, execution_context);
    let rsc_module_options_context =
        get_server_module_options_context(project_path, execution_context, ty, mode, next_config);

    Vc::upcast(
        NextServerComponentTransition {
            rsc_compile_time_info,
            rsc_module_options_context,
            rsc_resolve_options_context,
            server_root,
        }
        .cell(),
    )
}

#[turbo_tasks::function]
fn next_edge_route_transition(
    project_path: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
    output_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
) -> Vc<Box<dyn Transition>> {
    let server_ty = Value::new(ServerContextType::AppRoute { app_dir });

    let edge_compile_time_info = get_edge_compile_time_info(
        project_path,
        server_addr,
        Value::new(EnvironmentIntention::Api),
    );

    let edge_chunking_context = DevChunkingContext::builder(
        project_path,
        output_path.join("edge".to_string()),
        output_path.join("edge/chunks".to_string()),
        get_client_assets_path(server_root, Value::new(ClientContextType::App { app_dir })),
        edge_compile_time_info.environment(),
    )
    .reference_chunk_source_maps(should_debug("app_source"))
    .build();
    let edge_resolve_options_context =
        get_edge_resolve_options_context(project_path, server_ty, next_config, execution_context);

    Vc::upcast(
        NextEdgeRouteTransition {
            edge_compile_time_info,
            edge_chunking_context,
            edge_module_options_context: None,
            edge_resolve_options_context,
            output_path,
            base_path: app_dir,
            bootstrap_asset: next_asset("entry/app/edge-route-bootstrap.ts".to_string()),
            entry_name: "edge".to_string(),
        }
        .cell(),
    )
}

#[turbo_tasks::function]
fn next_edge_page_transition(
    project_path: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
    output_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
) -> Vc<Box<dyn Transition>> {
    let server_ty = Value::new(ServerContextType::AppRoute { app_dir });

    let edge_compile_time_info = get_edge_compile_time_info(
        project_path,
        server_addr,
        Value::new(EnvironmentIntention::ServerRendering),
    );

    let edge_chunking_context = DevChunkingContext::builder(
        project_path,
        output_path.join("edge-pages".into()),
        output_path.join("edge-pages/chunks".into()),
        get_client_assets_path(server_root, Value::new(ClientContextType::App { app_dir })),
        edge_compile_time_info.environment(),
    )
    .layer("ssr")
    .reference_chunk_source_maps(should_debug("app_source"))
    .build();
    let edge_resolve_options_context =
        get_edge_resolve_options_context(project_path, server_ty, next_config, execution_context);

    Vc::upcast(
        NextEdgePageTransition {
            edge_compile_time_info,
            edge_chunking_context,
            edge_module_options_context: None,
            edge_resolve_options_context,
            output_path,
            bootstrap_asset: next_asset("entry/app/edge-page-bootstrap.ts".to_string()),
        }
        .cell(),
    )
}

#[allow(clippy::too_many_arguments)]
#[turbo_tasks::function]
fn app_context(
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    server_root: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    client_compile_time_info: Vc<CompileTimeInfo>,
    ssr: bool,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
    output_path: Vc<FileSystemPath>,
) -> Vc<Box<dyn AssetContext>> {
    let next_server_to_client_transition = Vc::upcast(NextServerToClientTransition { ssr }.cell());
    let mode = NextMode::Development;

    let mut transitions = HashMap::new();
    transitions.insert(
        "next-edge-route".to_string(),
        next_edge_route_transition(
            project_path,
            app_dir,
            server_root,
            next_config,
            server_addr,
            output_path,
            execution_context,
        ),
    );
    transitions.insert(
        "next-edge-page".to_string(),
        next_edge_page_transition(
            project_path,
            app_dir,
            server_root,
            next_config,
            server_addr,
            output_path,
            execution_context,
        ),
    );
    transitions.insert(
        "next-server-component".to_string(),
        next_server_component_transition(
            project_path,
            execution_context,
            app_dir,
            server_root,
            env,
            next_config,
            server_addr,
        ),
    );
    transitions.insert(
        "next-edge-server-component".to_string(),
        next_edge_server_component_transition(
            project_path,
            execution_context,
            app_dir,
            server_root,
            next_config,
            server_addr,
        ),
    );
    transitions.insert(
        "server-to-client".to_string(),
        next_server_to_client_transition,
    );
    transitions.insert(
        "next-client".to_string(),
        next_client_transition(
            project_path,
            execution_context,
            server_root,
            app_dir,
            env,
            client_compile_time_info,
            next_config,
        ),
    );
    let client_ty = Value::new(ClientContextType::App { app_dir });
    transitions.insert(
        "next-client-chunks".to_string(),
        Vc::upcast(NextClientChunksTransition::new(
            project_path,
            execution_context,
            client_ty,
            mode,
            server_root,
            client_compile_time_info,
            next_config,
        )),
    );
    transitions.insert(
        "next-ssr-client-module".to_string(),
        next_ssr_client_module_transition(
            project_path,
            execution_context,
            app_dir,
            env,
            next_config,
            server_addr,
        ),
    );

    let ssr_ty = Value::new(ServerContextType::AppSSR { app_dir });
    Vc::upcast(ModuleAssetContext::new(
        Vc::cell(transitions),
        get_server_compile_time_info(ssr_ty, mode, env, server_addr),
        get_server_module_options_context(
            project_path,
            execution_context,
            ssr_ty,
            mode,
            next_config,
        ),
        get_server_resolve_options_context(
            project_path,
            ssr_ty,
            mode,
            next_config,
            execution_context,
        ),
    ))
}

/// Create a content source serving the `app` or `src/app` directory as
/// Next.js app folder.
#[turbo_tasks::function]
pub async fn create_app_source(
    app_dir: Vc<OptionAppDir>,
    project_path: Vc<FileSystemPath>,
    execution_context: Vc<ExecutionContext>,
    output_path: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    browserslist_query: String,
    next_config: Vc<NextConfig>,
    server_addr: Vc<ServerAddr>,
) -> Result<Vc<Box<dyn ContentSource>>> {
    let Some(app_dir) = *app_dir.await? else {
        return Ok(Vc::upcast(NoContentSource::new()));
    };
    let entrypoints = get_entrypoints(app_dir, next_config.page_extensions());
    let metadata = get_global_metadata(app_dir, next_config.page_extensions());

    let client_compile_time_info =
        get_client_compile_time_info(NextMode::Development, browserslist_query);

    let context_ssr = app_context(
        project_path,
        execution_context,
        server_root,
        app_dir,
        env,
        client_compile_time_info,
        true,
        next_config,
        server_addr,
        output_path,
    );
    let context = app_context(
        project_path,
        execution_context,
        server_root,
        app_dir,
        env,
        client_compile_time_info,
        false,
        next_config,
        server_addr,
        output_path,
    );

    let injected_env = env_for_js(Vc::upcast(EnvMap::empty()), false, next_config);
    let env = Vc::upcast(CustomProcessEnv::new(env, next_config.env()));

    let server_runtime_entries = Vc::cell(vec![Vc::upcast(ProcessEnvAsset::new(
        project_path,
        injected_env,
    ))]);

    let fallback_page = get_fallback_page(
        project_path,
        execution_context,
        server_root,
        env,
        client_compile_time_info,
        next_config,
    );
    let render_data = render_data(next_config, server_addr);

    let entrypoints = entrypoints.await?;
    let mut sources: Vec<_> = entrypoints
        .iter()
        .map(|(pathname, &loader_tree)| match loader_tree {
            Entrypoint::AppPage { loader_tree } => create_app_page_source_for_route(
                pathname.clone(),
                loader_tree,
                context_ssr,
                context,
                project_path,
                app_dir,
                env,
                server_root,
                server_runtime_entries,
                fallback_page,
                output_path,
                render_data,
            ),
            Entrypoint::AppRoute { path } => create_app_route_source_for_route(
                pathname.clone(),
                path,
                context_ssr,
                project_path,
                app_dir,
                env,
                server_root,
                server_runtime_entries,
                output_path,
                render_data,
            ),
        })
        .chain(once(create_global_metadata_source(
            app_dir,
            metadata,
            server_root,
        )))
        .collect();

    if let Some(&Entrypoint::AppPage { loader_tree }) = entrypoints.get("/") {
        if loader_tree.await?.components.await?.not_found.is_some() {
            // Only add a source for the app 404 page if a top-level not-found page is
            // defined. Otherwise, the 404 page is handled by the pages logic.
            let not_found_page_source = create_app_not_found_page_source(
                loader_tree,
                context_ssr,
                context,
                project_path,
                app_dir,
                env,
                server_root,
                server_runtime_entries,
                fallback_page,
                output_path,
                render_data,
            );
            sources.push(not_found_page_source);
        }
    }

    Ok(Vc::upcast(CombinedContentSource { sources }.cell()))
}

#[turbo_tasks::function]
async fn create_global_metadata_source(
    app_dir: Vc<FileSystemPath>,
    metadata: Vc<GlobalMetadata>,
    server_root: Vc<FileSystemPath>,
) -> Result<Vc<Box<dyn ContentSource>>> {
    let metadata = metadata.await?;
    let mut unsupported_metadata = Vec::new();
    let mut sources = Vec::new();
    for (server_path, item) in [
        ("robots.txt", metadata.robots),
        ("favicon.ico", metadata.favicon),
        ("sitemap.xml", metadata.sitemap),
    ] {
        let Some(item) = item else {
            continue;
        };
        match item {
            MetadataItem::Static { path } => {
                let asset = FixedStaticAsset::new(
                    server_root.join(server_path.to_string()),
                    Vc::upcast(SourceAsset::new(path)),
                );
                sources.push(Vc::upcast(AssetGraphContentSource::new_eager(
                    server_root,
                    Vc::upcast(asset),
                )))
            }
            MetadataItem::Dynamic { path } => {
                unsupported_metadata.push(path);
            }
        }
    }
    if !unsupported_metadata.is_empty() {
        UnsupportedDynamicMetadataIssue {
            app_dir,
            files: unsupported_metadata,
        }
        .cell()
        .emit();
    }
    Ok(Vc::upcast(CombinedContentSource { sources }.cell()))
}

#[allow(clippy::too_many_arguments)]
#[turbo_tasks::function]
async fn create_app_page_source_for_route(
    pathname: String,
    loader_tree: Vc<LoaderTree>,
    context_ssr: Vc<Box<dyn AssetContext>>,
    context: Vc<Box<dyn AssetContext>>,
    project_path: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    server_root: Vc<FileSystemPath>,
    runtime_entries: Vc<Assets>,
    fallback_page: Vc<DevHtmlAsset>,
    intermediate_output_path_root: Vc<FileSystemPath>,
    render_data: Vc<JsonValue>,
) -> Result<Vc<Box<dyn ContentSource>>> {
    let pathname_vc = Vc::cell(pathname.clone());

    let params_matcher = NextParamsMatcher::new(pathname_vc);

    let source = create_node_rendered_source(
        project_path,
        env,
        pathname_to_specificity(pathname.clone()),
        server_root,
        Vc::upcast(params_matcher),
        pathname_vc,
        Vc::upcast(
            AppRenderer {
                runtime_entries,
                app_dir,
                context_ssr,
                context,
                server_root,
                project_path,
                intermediate_output_path: intermediate_output_path_root,
                loader_tree,
            }
            .cell(),
        ),
        fallback_page,
        render_data,
        should_debug("app_source"),
    );

    Ok(source.issue_context(app_dir, format!("Next.js App Page Route {pathname}")))
}

#[allow(clippy::too_many_arguments)]
#[turbo_tasks::function]
async fn create_app_not_found_page_source(
    loader_tree: Vc<LoaderTree>,
    context_ssr: Vc<Box<dyn AssetContext>>,
    context: Vc<Box<dyn AssetContext>>,
    project_path: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    server_root: Vc<FileSystemPath>,
    runtime_entries: Vc<Assets>,
    fallback_page: Vc<DevHtmlAsset>,
    intermediate_output_path_root: Vc<FileSystemPath>,
    render_data: Vc<JsonValue>,
) -> Result<Vc<Box<dyn ContentSource>>> {
    let pathname_vc = Vc::cell("/404".to_string());

    let source = create_node_rendered_source(
        project_path,
        env,
        Specificity::not_found(),
        server_root,
        Vc::upcast(NextFallbackMatcher::new()),
        pathname_vc,
        Vc::upcast(
            AppRenderer {
                runtime_entries,
                app_dir,
                context_ssr,
                context,
                server_root,
                project_path,
                intermediate_output_path: intermediate_output_path_root,
                loader_tree,
            }
            .cell(),
        ),
        fallback_page,
        render_data,
        should_debug("app_source"),
    );

    Ok(source.issue_context(app_dir, "Next.js App Page Route /404".to_string()))
}

#[allow(clippy::too_many_arguments)]
#[turbo_tasks::function]
async fn create_app_route_source_for_route(
    pathname: String,
    entry_path: Vc<FileSystemPath>,
    context_ssr: Vc<Box<dyn AssetContext>>,
    project_path: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
    env: Vc<Box<dyn ProcessEnv>>,
    server_root: Vc<FileSystemPath>,
    runtime_entries: Vc<Assets>,
    intermediate_output_path_root: Vc<FileSystemPath>,
    render_data: Vc<JsonValue>,
) -> Result<Vc<Box<dyn ContentSource>>> {
    let pathname_vc = Vc::cell(pathname.to_string());

    let params_matcher = NextParamsMatcher::new(pathname_vc);

    let source = create_node_api_source(
        project_path,
        env,
        pathname_to_specificity(pathname.clone()),
        server_root,
        Vc::upcast(params_matcher),
        pathname_vc,
        Vc::upcast(
            AppRoute {
                context: context_ssr,
                runtime_entries,
                server_root,
                entry_path,
                project_path,
                intermediate_output_path: intermediate_output_path_root,
                output_root: intermediate_output_path_root,
                app_dir,
            }
            .cell(),
        ),
        render_data,
        should_debug("app_source"),
    );

    Ok(source.issue_context(app_dir, format!("Next.js App Route {pathname}")))
}

/// The renderer for pages in app directory
#[turbo_tasks::value]
struct AppRenderer {
    runtime_entries: Vc<Assets>,
    app_dir: Vc<FileSystemPath>,
    context_ssr: Vc<Box<dyn AssetContext>>,
    context: Vc<Box<dyn AssetContext>>,
    project_path: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    intermediate_output_path: Vc<FileSystemPath>,
    loader_tree: Vc<LoaderTree>,
}

#[turbo_tasks::value_impl]
impl AppRenderer {
    #[turbo_tasks::function]
    async fn entry(self: Vc<Self>, is_rsc: bool) -> Result<Vc<NodeRenderingEntry>> {
        let AppRenderer {
            runtime_entries,
            app_dir,
            context_ssr,
            context,
            project_path,
            server_root,
            intermediate_output_path,
            loader_tree,
        } = *self.await?;

        let (context, intermediate_output_path) = if is_rsc {
            (context, intermediate_output_path.join("rsc".to_string()))
        } else {
            (context_ssr, intermediate_output_path)
        };

        let config = parse_segment_config_from_loader_tree(loader_tree, context);

        let runtime = config.await?.runtime;
        let rsc_transition = match runtime {
            Some(NextRuntime::NodeJs) | None => "next-server-component",
            Some(NextRuntime::Edge) => "next-edge-server-component",
        };

        struct State {
            inner_assets: IndexMap<String, Vc<Box<dyn Asset>>>,
            counter: usize,
            imports: Vec<String>,
            loader_tree_code: String,
            context: Vc<Box<dyn AssetContext>>,
            unsupported_metadata: Vec<Vc<FileSystemPath>>,
            rsc_transition: &'static str,
        }

        impl State {
            fn unique_number(&mut self) -> usize {
                let i = self.counter;
                self.counter += 1;
                i
            }
        }

        let mut state = State {
            inner_assets: IndexMap::new(),
            counter: 0,
            imports: Vec::new(),
            loader_tree_code: String::new(),
            context,
            unsupported_metadata: Vec::new(),
            rsc_transition,
        };

        fn write_component(
            state: &mut State,
            name: &str,
            component: Option<Vc<FileSystemPath>>,
        ) -> Result<()> {
            use std::fmt::Write;

            if let Some(component) = component {
                let i = state.unique_number();
                let identifier = magic_identifier::mangle(&format!("{name} #{i}"));
                let chunks_identifier = magic_identifier::mangle(&format!("chunks of {name} #{i}"));
                writeln!(
                    state.loader_tree_code,
                    "  {name}: [() => {identifier}, JSON.stringify({chunks_identifier}) + '.js'],",
                    name = StringifyJs(name)
                )?;
                state.imports.push(format!(
                    r#"("TURBOPACK {{ chunking-type: isolatedParallel }}");
import {}, {{ chunks as {} }} from "COMPONENT_{}";
"#,
                    identifier, chunks_identifier, i
                ));

                state.inner_assets.insert(
                    format!("COMPONENT_{i}"),
                    state
                        .context
                        .with_transition(state.rsc_transition.to_string())
                        .process(
                            Vc::upcast(SourceAsset::new(component)),
                            Value::new(ReferenceType::EcmaScriptModules(
                                EcmaScriptModulesReferenceSubType::Undefined,
                            )),
                        ),
                );
            }
            Ok(())
        }

        fn write_metadata(state: &mut State, metadata: &Metadata) -> Result<()> {
            if metadata.is_empty() {
                return Ok(());
            }
            let Metadata {
                icon,
                apple,
                twitter,
                open_graph,
                favicon,
                manifest,
            } = metadata;
            state.loader_tree_code += "  metadata: {";
            write_metadata_items(state, "icon", favicon.iter().chain(icon.iter()))?;
            write_metadata_items(state, "apple", apple.iter())?;
            write_metadata_items(state, "twitter", twitter.iter())?;
            write_metadata_items(state, "openGraph", open_graph.iter())?;
            write_metadata_manifest(state, *manifest)?;
            state.loader_tree_code += "  },";
            Ok(())
        }

        fn write_metadata_manifest(
            state: &mut State,
            manifest: Option<MetadataItem>,
        ) -> Result<()> {
            let Some(manifest) = manifest else {
                return Ok(());
            };
            match manifest {
                MetadataItem::Static { path } => {
                    use std::fmt::Write;
                    let i = state.unique_number();
                    let identifier = magic_identifier::mangle(&format!("manifest #{i}"));
                    let inner_module_id = format!("METADATA_{i}");
                    state
                        .imports
                        .push(format!("import {identifier} from \"{inner_module_id}\";"));
                    state.inner_assets.insert(
                        inner_module_id,
                        Vc::upcast(StaticModuleAsset::new(
                            Vc::upcast(SourceAsset::new(path)),
                            state.context,
                        )),
                    );
                    writeln!(state.loader_tree_code, "    manifest: {identifier},")?;
                }
                MetadataItem::Dynamic { path } => {
                    state.unsupported_metadata.push(path);
                }
            }

            Ok(())
        }

        fn write_metadata_items<'a>(
            state: &mut State,
            name: &str,
            it: impl Iterator<Item = &'a MetadataWithAltItem>,
        ) -> Result<()> {
            use std::fmt::Write;
            let mut it = it.peekable();
            if it.peek().is_none() {
                return Ok(());
            }
            writeln!(state.loader_tree_code, "    {name}: [")?;
            for item in it {
                write_metadata_item(state, name, item)?;
            }
            writeln!(state.loader_tree_code, "    ],")?;
            Ok(())
        }

        fn write_metadata_item(
            state: &mut State,
            name: &str,
            item: &MetadataWithAltItem,
        ) -> Result<()> {
            use std::fmt::Write;
            let i = state.unique_number();
            let identifier = magic_identifier::mangle(&format!("{name} #{i}"));
            let inner_module_id = format!("METADATA_{i}");
            state
                .imports
                .push(format!("import {identifier} from \"{inner_module_id}\";"));
            let s = "      ";
            match item {
                MetadataWithAltItem::Static { path, alt_path } => {
                    state.inner_assets.insert(
                        inner_module_id,
                        StructuredImageModuleType::create_module(
                            Vc::upcast(SourceAsset::new(*path)),
                            BlurPlaceholderMode::None,
                            state.context,
                        ),
                    );
                    writeln!(state.loader_tree_code, "{s}(async (props) => [{{")?;
                    writeln!(state.loader_tree_code, "{s}  url: {identifier}.src,")?;
                    let numeric_sizes = name == "twitter" || name == "openGraph";
                    if numeric_sizes {
                        writeln!(state.loader_tree_code, "{s}  width: {identifier}.width,")?;
                        writeln!(state.loader_tree_code, "{s}  height: {identifier}.height,")?;
                    } else {
                        writeln!(
                            state.loader_tree_code,
                            "{s}  sizes: `${{{identifier}.width}}x${{{identifier}.height}}`,"
                        )?;
                    }
                    if let Some(alt_path) = alt_path {
                        let identifier = magic_identifier::mangle(&format!("{name} alt text #{i}"));
                        let inner_module_id = format!("METADATA_ALT_{i}");
                        state
                            .imports
                            .push(format!("import {identifier} from \"{inner_module_id}\";"));
                        state.inner_assets.insert(
                            inner_module_id,
                            state.context.process(
                                Vc::upcast(TextContentSourceAsset::new(Vc::upcast(
                                    SourceAsset::new(*alt_path),
                                ))),
                                Value::new(ReferenceType::Internal(InnerAssets::empty())),
                            ),
                        );
                        writeln!(state.loader_tree_code, "{s}  alt: {identifier},")?;
                    }
                    writeln!(state.loader_tree_code, "{s}}}]),")?;
                }
                MetadataWithAltItem::Dynamic { path, .. } => {
                    state.unsupported_metadata.push(*path);
                }
            }
            Ok(())
        }

        #[async_recursion]
        async fn walk_tree(state: &mut State, loader_tree: Vc<LoaderTree>) -> Result<()> {
            use std::fmt::Write;

            let LoaderTree {
                segment,
                parallel_routes,
                components,
            } = &*loader_tree.await?;

            writeln!(
                state.loader_tree_code,
                "[{segment}, {{",
                segment = StringifyJs(segment)
            )?;
            // add parallel_routers
            for (key, &parallel_route) in parallel_routes.iter() {
                write!(state.loader_tree_code, "{key}: ", key = StringifyJs(key))?;
                walk_tree(state, parallel_route).await?;
                writeln!(state.loader_tree_code, ",")?;
            }
            writeln!(state.loader_tree_code, "}}, {{")?;
            // add components
            let Components {
                page,
                default,
                error,
                layout,
                loading,
                template,
                not_found,
                metadata,
                route: _,
            } = &*components.await?;
            write_component(state, "page", *page)?;
            write_component(state, "defaultPage", *default)?;
            write_component(state, "error", *error)?;
            write_component(state, "layout", *layout)?;
            write_component(state, "loading", *loading)?;
            write_component(state, "template", *template)?;
            write_component(state, "not-found", *not_found)?;
            write_metadata(state, metadata)?;
            write!(state.loader_tree_code, "}}]")?;
            Ok(())
        }

        walk_tree(&mut state, loader_tree).await?;

        let State {
            inner_assets,
            imports,
            loader_tree_code,
            unsupported_metadata,
            ..
        } = state;

        if !unsupported_metadata.is_empty() {
            UnsupportedDynamicMetadataIssue {
                app_dir,
                files: unsupported_metadata,
            }
            .cell()
            .emit();
        }

        let mut result = RopeBuilder::from(indoc! {"
                \"TURBOPACK { chunking-type: isolatedParallel; transition: next-edge-server-component }\";
                import GlobalErrorMod from \"next/dist/client/components/error-boundary\"
                const { GlobalError } = GlobalErrorMod;
                \"TURBOPACK { chunking-type: isolatedParallel; transition: next-edge-server-component }\";
                import base from \"next/dist/server/app-render/entry-base\"\n
            "});

        for import in imports {
            writeln!(result, "{import}")?;
        }

        writeln!(result, "const tree = {loader_tree_code};\n")?;
        writeln!(result, "const pathname = '';\n")?;
        writeln!(
            result,
            // Need this hack because "export *" from CommonJS will trigger a warning
            // otherwise
            "__turbopack_export_value__({{ tree, GlobalError, pathname, ...base }});\n"
        )?;

        let file = File::from(result.build());
        let asset = VirtualAsset::new(
            next_js_file_path("entry/app-entry.tsx".to_string()),
            AssetContent::file(file.into()),
        );

        let chunking_context = DevChunkingContext::builder(
            project_path,
            intermediate_output_path,
            intermediate_output_path.join("chunks".to_string()),
            get_client_assets_path(server_root, Value::new(ClientContextType::App { app_dir })),
            context.compile_time_info().environment(),
        )
        .layer("ssr")
        .reference_chunk_source_maps(should_debug("app_source"))
        .build();

        let renderer_module = match runtime {
            Some(NextRuntime::NodeJs) | None => context.process(
                Vc::upcast(SourceAsset::new(next_js_file_path("entry/app-renderer.tsx".to_string()))),
                Value::new(ReferenceType::Internal(Vc::cell(indexmap! {
                    "APP_ENTRY".to_string() => context.with_transition(rsc_transition.to_string()).process(
                        Vc::upcast(asset),
                        Value::new(ReferenceType::Internal(Vc::cell(inner_assets))),
                    ),
                    "APP_BOOTSTRAP".to_string() => context.with_transition("next-client".to_string()).process(
                        Vc::upcast(SourceAsset::new(next_js_file_path("entry/app/hydrate.tsx".to_string()))),
                        Value::new(ReferenceType::EcmaScriptModules(
                            EcmaScriptModulesReferenceSubType::Undefined,
                        )),
                    ),
                }))),
            ),
            Some(NextRuntime::Edge) =>
                context.process(
                    Vc::upcast(SourceAsset::new(next_js_file_path("entry/app-edge-renderer.tsx".to_string()))),
                    Value::new(ReferenceType::Internal(Vc::cell(indexmap! {
                        "INNER_EDGE_CHUNK_GROUP".to_string() => context.with_transition("next-edge-page".to_string()).process(
                            Vc::upcast(asset),
                            Value::new(ReferenceType::Internal(Vc::cell(inner_assets))),
                        ),
                    }))),
                )
        };

        let Some(module) = Vc::try_resolve_sidecast::<Box<dyn EvaluatableAsset>>(renderer_module).await? else {
            bail!("internal module must be evaluatable");
        };

        Ok(NodeRenderingEntry {
            runtime_entries: Vc::cell(
                runtime_entries
                    .await?
                    .iter()
                    .map(|entry| entry.to_evaluatable(context))
                    .collect(),
            ),
            module,
            chunking_context,
            intermediate_output_path,
            output_root: intermediate_output_path.root(),
            project_dir: project_path,
        }
        .cell())
    }
}

#[turbo_tasks::value_impl]
impl NodeEntry for AppRenderer {
    #[turbo_tasks::function]
    fn entry(self: Vc<Self>, data: Value<ContentSourceData>) -> Vc<NodeRenderingEntry> {
        let data = data.into_value();
        let is_rsc = if let Some(headers) = data.headers {
            headers.contains_key("rsc")
        } else {
            false
        };
        // Call with only is_rsc as key
        self.entry(is_rsc)
    }
}

/// The node.js renderer api routes in the app directory
#[turbo_tasks::value]
struct AppRoute {
    runtime_entries: Vc<Assets>,
    context: Vc<Box<dyn AssetContext>>,
    entry_path: Vc<FileSystemPath>,
    intermediate_output_path: Vc<FileSystemPath>,
    project_path: Vc<FileSystemPath>,
    server_root: Vc<FileSystemPath>,
    output_root: Vc<FileSystemPath>,
    app_dir: Vc<FileSystemPath>,
}

#[turbo_tasks::value_impl]
impl AppRoute {
    #[turbo_tasks::function]
    async fn entry(self: Vc<Self>) -> Result<Vc<NodeRenderingEntry>> {
        let this = self.await?;

        let chunking_context = DevChunkingContext::builder(
            this.project_path,
            this.intermediate_output_path,
            this.intermediate_output_path.join("chunks".to_string()),
            get_client_assets_path(
                this.server_root,
                Value::new(ClientContextType::App {
                    app_dir: this.app_dir,
                }),
            ),
            this.context.compile_time_info().environment(),
        )
        .layer("ssr")
        .reference_chunk_source_maps(should_debug("app_source"))
        .build();

        let entry_source_asset = SourceAsset::new(this.entry_path);
        let entry_asset = this.context.process(
            Vc::upcast(entry_source_asset),
            Value::new(ReferenceType::Entry(EntryReferenceSubType::AppRoute)),
        );

        let config = parse_segment_config_from_source(entry_asset);
        let module = match config.await?.runtime {
            Some(NextRuntime::NodeJs) | None => {
                let bootstrap_asset = next_asset("entry/app/route.ts".to_string());

                route_bootstrap(
                    entry_asset,
                    this.context,
                    this.project_path,
                    bootstrap_asset,
                    BootstrapConfig::empty(),
                )
            }
            Some(NextRuntime::Edge) => {
                let internal_asset = next_asset("entry/app/edge-route.ts".to_string());

                let entry = this
                    .context
                    .with_transition("next-edge-route".to_string())
                    .process(
                        Vc::upcast(entry_source_asset),
                        Value::new(ReferenceType::Entry(EntryReferenceSubType::AppRoute)),
                    );

                let module = this.context.process(
                    internal_asset,
                    Value::new(ReferenceType::Internal(Vc::cell(indexmap! {
                        "ROUTE_CHUNK_GROUP".to_string() => entry
                    }))),
                );

                let Some(module) = Vc::try_resolve_sidecast::<Box<dyn EvaluatableAsset>>(module).await? else {
                    bail!("internal module must be evaluatable");
                };

                module
            }
        };

        Ok(NodeRenderingEntry {
            runtime_entries: Vc::cell(
                this.runtime_entries
                    .await?
                    .iter()
                    .map(|entry| entry.to_evaluatable(this.context))
                    .collect(),
            ),
            module,
            chunking_context,
            intermediate_output_path: this.intermediate_output_path,
            output_root: this.output_root,
            project_dir: this.project_path,
        }
        .cell())
    }
}

#[turbo_tasks::value_impl]
impl NodeEntry for AppRoute {
    #[turbo_tasks::function]
    fn entry(self: Vc<Self>, _data: Value<ContentSourceData>) -> Vc<NodeRenderingEntry> {
        // Call without being keyed by data
        self.entry()
    }
}

#[turbo_tasks::value]
struct UnsupportedDynamicMetadataIssue {
    app_dir: Vc<FileSystemPath>,
    files: Vec<Vc<FileSystemPath>>,
}

#[turbo_tasks::value_impl]
impl Issue for UnsupportedDynamicMetadataIssue {
    #[turbo_tasks::function]
    fn severity(&self) -> Vc<IssueSeverity> {
        IssueSeverity::Warning.into()
    }

    #[turbo_tasks::function]
    fn category(&self) -> Vc<String> {
        Vc::cell("unsupported".to_string())
    }

    #[turbo_tasks::function]
    fn context(&self) -> Vc<FileSystemPath> {
        self.app_dir
    }

    #[turbo_tasks::function]
    fn title(&self) -> Vc<String> {
        Vc::cell(
            "Dynamic metadata from filesystem is currently not supported in Turbopack".to_string(),
        )
    }

    #[turbo_tasks::function]
    async fn description(&self) -> Result<Vc<String>> {
        let mut files = self
            .files
            .iter()
            .map(|file| file.to_string())
            .try_join()
            .await?;
        files.sort();
        Ok(Vc::cell(format!(
            "The following files were found in the app directory, but are not supported by \
             Turbopack. They are ignored:\n{}",
            FormatIter(|| files.iter().flat_map(|file| vec!["\n- ", file]))
        )))
    }
}
