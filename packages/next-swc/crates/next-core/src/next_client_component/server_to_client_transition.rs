use anyhow::Result;
use indexmap::indexmap;
use turbo_tasks::{Value, Vc};
use turbopack_binding::turbopack::{
    core::{
        asset::Asset,
        context::AssetContext,
        reference_type::{EntryReferenceSubType, InnerAssets, ReferenceType},
    },
    turbopack::{transition::Transition, ModuleAssetContext},
};

use crate::embed_js::next_asset;

#[turbo_tasks::value(shared)]
pub struct NextServerToClientTransition {
    pub ssr: bool,
}

#[turbo_tasks::value_impl]
impl Transition for NextServerToClientTransition {
    #[turbo_tasks::function]
    async fn process(
        self: Vc<Self>,
        asset: Vc<Box<dyn Asset>>,
        context: Vc<ModuleAssetContext>,
        _reference_type: Value<ReferenceType>,
    ) -> Result<Vc<Box<dyn Asset>>> {
        let internal_asset = next_asset(if self.await?.ssr {
            "entry/app/server-to-client-ssr.tsx"
        } else {
            "entry/app/server-to-client.tsx"
        });
        let context = self.process_context(context);
        let client_chunks = context.with_transition("next-client-chunks").process(
            asset,
            Value::new(ReferenceType::Entry(
                EntryReferenceSubType::AppClientComponent,
            )),
        );
        let client_module = context.with_transition("next-ssr-client-module").process(
            asset,
            Value::new(ReferenceType::Entry(
                EntryReferenceSubType::AppClientComponent,
            )),
        );
        Ok(context.process(
            internal_asset,
            Value::new(ReferenceType::Internal(Vc::cell(indexmap! {
                "CLIENT_MODULE".to_string() => client_module,
                "CLIENT_CHUNKS".to_string() => client_chunks,
            }))),
        ))
    }
}
