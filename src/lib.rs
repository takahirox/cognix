//! Reusable agent engine. Terminal presentation and approval live in the binary.
mod action;
pub mod agent;
pub mod cli;
pub mod commands;
pub mod provider;
pub mod runtime;
pub mod schema;
pub mod terminal;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_canceled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub async fn wait(&self) {
        while !self.is_canceled() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

#[derive(Debug, Clone)]
pub enum Error {
    Canceled,
    Limit(String),
    Context(String),
    Provider(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Canceled => write!(f, "Interrupted; subsequent actions were not run"),
            Self::Limit(s) | Self::Context(s) | Self::Provider(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for Error {}
