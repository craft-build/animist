#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use animist::{
    catalog::{Catalog, Error},
    provider::{Effort, Tier},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const FIXTURE: &[u8] = include_bytes!("fixtures/models-dev.json");

#[test]
fn populates_model_info_without_inventing_capabilities() {
    let catalog = Catalog::from_slice(FIXTURE).unwrap();
    let model = catalog.model("example", "reasoner").unwrap();
    assert_eq!(model.name, "Example Reasoner");
    assert_eq!(model.info.ids, ["reasoner"]);
    assert_eq!(model.info.tier, Tier::Medium);
    assert!(model.info.features.vision);
    assert!(model.info.features.thinking.supported);
    assert!(!model.info.features.thinking.required);
    assert_eq!(
        model.info.features.thinking.efforts,
        [Effort::Low, Effort::High, Effort::XHigh, Effort::Max]
    );
    assert!(!model.info.features.tool_examples);
    assert!(!model.info.features.deferred_tools);
    assert_eq!(model.info.config.context_window, Some(200_000));
    assert_eq!(model.info.config.max_output_tokens, Some(20000));
    assert_eq!(model.info.pricing.input, Some(3.0));
    assert_eq!(model.info.pricing.output, Some(15.0));
    assert_eq!(model.info.pricing.cache_read, Some(0.3));
    assert_eq!(model.info.pricing.cache_write, Some(3.75));
    assert_eq!(model.info.pricing.fast, None);

    let budget = &catalog.model("example", "budget-only").unwrap().info;
    assert!(budget.features.thinking.supported);
    assert_eq!(budget.features.thinking.efforts, Vec::new());
    assert_eq!(budget.pricing.input, Some(1.0));
    assert_eq!(budget.pricing.cache_read, None);
    assert_eq!(budget.pricing.cache_write, None);

    let text = &catalog.model("example", "text-only").unwrap().info;
    assert!(!text.features.vision);
    assert!(!text.features.thinking.supported);
    assert_eq!(text.features.thinking.efforts, Vec::new());
    assert_eq!(text.pricing.input, None);
    assert_eq!(catalog.model("example", "unnamed").unwrap().name, "unnamed");
}

#[test]
fn preserves_provider_metadata_and_original_model_records() {
    let source: Value = serde_json::from_slice(FIXTURE).unwrap();
    let catalog = Catalog::from_slice(FIXTURE).unwrap();
    let provider = catalog.provider("example").unwrap();
    assert_eq!(provider.info.id, "example");
    assert_eq!(provider.info.env, ["EXAMPLE_API_KEY"]);
    assert_eq!(
        provider.info.api.as_deref(),
        Some("https://example.invalid/v1")
    );
    assert_eq!(provider.info.npm.as_deref(), Some("@ai-sdk/example"));
    assert_eq!(
        provider.info.extra["future_provider_field"],
        json!({"enabled": true})
    );
    assert!(!provider.info.extra.contains_key("models"));
    assert_eq!(
        provider.models["reasoner"].metadata,
        source["example"]["models"]["reasoner"]
    );
    assert_eq!(catalog.providers.len(), 2);
    assert_eq!(
        catalog
            .model("other", "reasoner")
            .unwrap()
            .info
            .pricing
            .input,
        Some(0.0)
    );
    assert_eq!(
        catalog
            .model("example", "reasoner")
            .unwrap()
            .info
            .pricing
            .input,
        Some(3.0)
    );
    assert!(catalog.model("missing", "reasoner").is_none());
    assert!(catalog.model("example", "missing").is_none());
    let encoded = serde_json::to_vec(&catalog).unwrap();
    assert_eq!(
        serde_json::from_slice::<Catalog>(&encoded).unwrap(),
        catalog
    );
}

#[test]
fn rejects_malformed_data_with_model_context() {
    assert!(matches!(
        Catalog::from_slice(b"invalid"),
        Err(Error::Decode { .. })
    ));
    let mut source: Value = serde_json::from_slice(FIXTURE).unwrap();
    source["example"]["models"]["reasoner"]["limit"]["output"] = json!("not a token count");
    let error = Catalog::from_slice(&serde_json::to_vec(&source).unwrap()).unwrap_err();
    assert!(matches!(
        error,
        Error::Model { provider, model, .. } if provider == "example" && model == "reasoner"
    ));
}

#[tokio::test]
async fn loads_saved_feed_asynchronously() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/models-dev.json"
    );
    assert_eq!(
        Catalog::load(path).await.unwrap(),
        Catalog::from_slice(FIXTURE).unwrap()
    );
    let missing = format!("{path}/missing.json");
    assert!(matches!(
        Catalog::load(missing).await,
        Err(Error::Read { .. })
    ));
}

async fn serve_once(status: &str, body: &[u8]) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let body = body.to_vec();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            assert!(socket.read_buf(&mut request).await.unwrap() > 0);
        }
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(&body).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn fetches_feed_and_rejects_http_errors() {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let (url, server) = serve_once("200 OK", FIXTURE).await;
    assert_eq!(
        Catalog::fetch_from(&client, &url).await.unwrap(),
        Catalog::from_slice(FIXTURE).unwrap()
    );
    server.await.unwrap();

    let (url, server) = serve_once("503 Service Unavailable", b"unavailable").await;
    let error = Catalog::fetch_from(&client, &url).await.unwrap_err();
    assert!(matches!(error, Error::Fetch { source, .. } if source.status().unwrap() == 503));
    server.await.unwrap();

    let (url, server) = serve_once("200 OK", b"invalid JSON").await;
    assert!(matches!(
        Catalog::fetch_from(&client, &url).await,
        Err(Error::Decode { .. })
    ));
    server.await.unwrap();
}

#[tokio::test]
#[ignore = "requires access to models.dev"]
async fn fetches_live_models_dev_catalog() {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();
    let catalog = Catalog::fetch(&client).await.unwrap();
    assert!(!catalog.providers.is_empty());
    assert!(!catalog.provider("anthropic").unwrap().models.is_empty());
    assert!(!catalog.provider("openai").unwrap().models.is_empty());
}
