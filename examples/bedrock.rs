mod support;

use animist::provider::{
    Provider,
    bedrock::{Bedrock, BedrockConfig},
};

#[tokio::main]
async fn main() -> Result<(), support::Error> {
    let Some(args) =
        support::Arguments::parse("bedrock", "global.anthropic.claude-sonnet-4-6").await?
    else {
        return Ok(());
    };
    let catalog = support::load_catalog("amazon-bedrock", args.catalog.as_deref()).await?;
    let models = catalog
        .models
        .into_values()
        .map(|model| model.info)
        .collect();
    let provider = Bedrock::new(
        BedrockConfig::builder()
            .maybe_endpoint_url(args.base_url.clone())
            .build(),
    )
    .await?;
    support::run(Provider::new(provider), models, args).await
}
