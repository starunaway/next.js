use std::collections::BTreeSet;

use anyhow::Result;
use turbo_tasks::{Value, Vc};
use turbo_tasks_fs::FileSystem;
use turbopack_binding::turbopack::{
    core::{
        asset::AssetContent, ident::AssetIdent, introspect::Introspectable,
        server_fs::ServerFileSystem, version::VersionedContent,
    },
    dev_server::source::{
        query::QueryValue,
        wrapping_source::{encode_pathname_to_url, ContentSourceProcessor, WrappedContentSource},
        ContentSource, ContentSourceContent, ContentSourceData, ContentSourceDataFilter,
        ContentSourceDataVary, ContentSourceResult, NeededData, ProxyResult, RewriteBuilder,
    },
    image::process::optimize,
};

/// Serves, resizes, optimizes, and re-encodes images to be used with
/// next/image.
#[turbo_tasks::value(shared)]
pub struct NextImageContentSource {
    asset_source: Vc<Box<dyn ContentSource>>,
}

#[turbo_tasks::value_impl]
impl NextImageContentSource {
    #[turbo_tasks::function]
    pub fn new(asset_source: Vc<Box<dyn ContentSource>>) -> Vc<NextImageContentSource> {
        NextImageContentSource { asset_source }.cell()
    }
}

#[turbo_tasks::value_impl]
impl ContentSource for NextImageContentSource {
    #[turbo_tasks::function]
    async fn get(
        self: Vc<Self>,
        path: String,
        data: Value<ContentSourceData>,
    ) -> Result<Vc<ContentSourceResult>> {
        let this = self.await?;

        let Some(query) = &data.query else {
            let queries = ["url".to_string(), "w".to_string(), "q".to_string()]
                .into_iter()
                .collect::<BTreeSet<_>>();

            return Ok(ContentSourceResult::need_data(Value::new(NeededData {
                source: self.into(),
                path: path.to_string(),
                vary: ContentSourceDataVary {
                    url: true,
                    query: Some(ContentSourceDataFilter::Subset(queries)),
                    ..Default::default()
                },
            })));
        };

        let Some(QueryValue::String(url)) = query.get("url") else {
            return Ok(ContentSourceResult::not_found());
        };

        let q = match query.get("q") {
            None => 75,
            Some(QueryValue::String(s)) => {
                let Ok(q) = s.parse::<u8>() else {
                    return Ok(ContentSourceResult::not_found());
                };
                q
            }
            _ => return Ok(ContentSourceResult::not_found()),
        };

        let w = match query.get("w") {
            Some(QueryValue::String(s)) => {
                let Ok(w) = s.parse::<u32>() else {
                    return Ok(ContentSourceResult::not_found());
                };
                w
            }
            _ => return Ok(ContentSourceResult::not_found()),
        };

        // TODO: re-encode into next-gen formats.
        if let Some(path) = url.strip_prefix('/') {
            let wrapped = WrappedContentSource::new(
                this.asset_source,
                Vc::upcast(NextImageContentSourceProcessor::new(path.to_string(), w, q)),
            );
            return Ok(ContentSourceResult::exact(
                ContentSourceContent::Rewrite(
                    RewriteBuilder::new(encode_pathname_to_url(path))
                        .content_source(Vc::upcast(wrapped))
                        .build(),
                )
                .cell()
                .into(),
            ));
        }

        // TODO: This should be downloaded by the server, and resized, etc.
        Ok(ContentSourceResult::exact(
            ContentSourceContent::HttpProxy(
                ProxyResult {
                    status: 302,
                    headers: vec![("Location".to_string(), url.clone())],
                    body: "".into(),
                }
                .cell(),
            )
            .cell()
            .into(),
        ))
    }
}

#[turbo_tasks::value_impl]
impl Introspectable for NextImageContentSource {
    #[turbo_tasks::function]
    fn ty(&self) -> Vc<String> {
        Vc::cell("next image content source".to_string())
    }

    #[turbo_tasks::function]
    fn details(&self) -> Vc<String> {
        Vc::cell("supports dynamic serving of any statically imported image".to_string())
    }
}

#[turbo_tasks::value]
struct NextImageContentSourceProcessor {
    path: String,
    width: u32,
    quality: u8,
}

#[turbo_tasks::value_impl]
impl NextImageContentSourceProcessor {
    #[turbo_tasks::function]
    pub fn new(path: String, width: u32, quality: u8) -> Vc<NextImageContentSourceProcessor> {
        NextImageContentSourceProcessor {
            path,
            width,
            quality,
        }
        .cell()
    }
}

#[turbo_tasks::value_impl]
impl ContentSourceProcessor for NextImageContentSourceProcessor {
    #[turbo_tasks::function]
    async fn process(&self, content: Vc<ContentSourceContent>) -> Result<Vc<ContentSourceContent>> {
        let ContentSourceContent::Static(static_content) = *content.await? else {
            return Ok(content);
        };
        let static_content = static_content.await?;
        let asset_content = static_content.content.content().await?;
        let AssetContent::File(file_content) = *asset_content else {
            return Ok(content);
        };
        let optimized_file_content = optimize(
            AssetIdent::from_path(ServerFileSystem::new().root().join(&self.path)),
            file_content,
            self.width,
            u32::MAX,
            self.quality,
        );
        Ok(ContentSourceContent::static_content(
            AssetContent::File(optimized_file_content).into(),
        ))
    }
}
