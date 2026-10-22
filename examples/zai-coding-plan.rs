mod support;

use animist::provider::{
    Provider,
    zai_coding_plan::{ZaiCodingPlan, ZaiCodingPlanConfig},
};

#[tokio::main]
async fn main() -> Result<(), support::Error> {
    let Some(args) = support::Arguments::parse("zai-coding-plan", "glm-5.3").await? else {
        return Ok(());
    };
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .base_url(
                args.base_url
                    .as_deref()
                    .unwrap_or(animist::provider::zai_coding_plan::BASE_URL),
            )
            .build(),
    )?;
    let catalog = support::load_catalog("zai-coding-plan", args.catalog.as_deref()).await?;
    let models = catalog
        .models
        .into_values()
        .map(|model| model.info)
        .collect();
    support::run(Provider::new(provider), models, args).await
}
