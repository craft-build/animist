use serde::Deserialize;
use serde_json::Value;

pub(super) const TEXT: &str = include_str!("../fixtures/bedrock-text.json");
pub(super) const TOOLS: &str = include_str!("../fixtures/bedrock-tools.json");
pub(super) const REASONING: &str = include_str!("../fixtures/bedrock-reasoning.json");

#[derive(Debug, Deserialize)]
pub(super) struct Event {
    pub event: String,
    pub payload: Value,
}

pub(super) fn fixture(source: &str) -> Vec<u8> {
    serde_json::from_str::<Vec<Event>>(source)
        .unwrap()
        .into_iter()
        .flat_map(|event| {
            frame(
                &[(":message-type", "event"), (":event-type", &event.event)],
                &serde_json::to_vec(&event.payload).unwrap(),
            )
        })
        .collect()
}

pub(super) fn frame(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for (name, value) in headers {
        encoded.push(u8::try_from(name.len()).unwrap());
        encoded.extend_from_slice(name.as_bytes());
        encoded.push(7);
        encoded.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        encoded.extend_from_slice(value.as_bytes());
    }
    raw_frame(&encoded, payload)
}

pub(super) fn raw_frame(headers: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        &u32::try_from(16 + headers.len() + payload.len())
            .unwrap()
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&u32::try_from(headers.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_be_bytes());
    bytes.extend_from_slice(headers);
    bytes.extend_from_slice(payload);
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_be_bytes());
    bytes
}
