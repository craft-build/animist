mod config;
mod error;
mod layer;
mod prompt;

use crate::{
    Struct,
    provider::{Model, TokenUsage},
};
use serde::{Deserialize, Serialize};
use std::sync::{
    RwLock,
    atomic::{AtomicU32, Ordering},
};
use tokio::sync::Mutex;

pub use error::AgentError;
pub use prompt::{Prompt, Section};

use crate::history::History;

type Result<T> = std::result::Result<T, error::AgentError>;

#[derive(Debug, Default)]
pub struct Ledger {
    context_size: AtomicU32,
    num_turns: AtomicU32,
    totals: Mutex<TokenUsage>,
}

impl Ledger {
    pub fn context_size(&self) -> u32 {
        self.context_size.load(Ordering::SeqCst)
    }

    pub fn add_tokens(&self, tokens: u32) {
        self.context_size.fetch_add(tokens, Ordering::SeqCst);
    }

    pub fn set_context(&self, tokens: u32) {
        self.context_size.store(tokens, Ordering::SeqCst);
    }

    pub fn num_turns(&self) -> u32 {
        self.num_turns.load(Ordering::SeqCst)
    }

    pub fn add_turns(&self, turns: u32) {
        self.num_turns.fetch_add(turns, Ordering::SeqCst);
    }

    pub fn set_turns(&self, turns: u32) {
        self.num_turns.store(turns, Ordering::SeqCst);
    }

    pub async fn totals(&self) -> TokenUsage {
        self.totals.lock().await.clone()
    }

    pub async fn add(&self, usage: TokenUsage) {
        let mut totals = self.totals.lock().await;
        totals.input += usage.input;
        totals.output += usage.output;
        totals.cache_creation += usage.cache_creation;
        totals.cache_read += usage.cache_read;
        totals.cost = usage
            .cost
            .map(|x| totals.cost.unwrap_or_default() + x)
            .or(totals.cost);
    }
}

pub struct Agent {
    prompt: RwLock<Prompt>,
    history: History,
    ledger: Ledger,
    model: RwLock<Model>,
}
