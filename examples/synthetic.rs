mod support;

use animist::provider::{
    Provider, ProviderImpl,
    synthetic::{Synthetic, SyntheticConfig},
};

#[tokio::main]
async fn main() -> Result<(), support::Error> {
    let Some(args) = support::Arguments::parse("synthetic", "syn:large:text").await? else {
        return Ok(());
    };
    let provider = Synthetic::new(
        SyntheticConfig::builder()
            .base_url(
                args.base_url
                    .as_deref()
                    .unwrap_or(animist::provider::synthetic::BASE_URL),
            )
            .build(),
    )?;
    let models = if args.catalog.is_some() {
        support::load_catalog("synthetic", args.catalog.as_deref())
            .await?
            .models
            .into_values()
            .map(|model| model.info)
            .collect()
    } else {
        provider.list_models().await?
    };
    support::run(Provider::new(provider), models, args).await
}
