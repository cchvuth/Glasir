//! AI helpers using the user's own Claude or ChatGPT token: commit messages,
//! PR descriptions, conflict resolution and Commit Composer plans.
//! `client` (settings, tokens, Claude API), `openai` (OpenAI API), `tasks` (prompts, jobs).

mod cli;
mod client;
mod openai;
mod tasks;

pub use client::{Config, IN_FILE, IN_KEYCHAIN, Provider, env_source, find_saved, remove_plain_token, save_plain_token, save_token, tls};
pub use tasks::{Job, MAX_CONTEXT, PRESETS, Purpose, secret, start, test, too_large};

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(true);
static PROVIDER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Settings → "Enable AI features", mirrored every frame for UI code without the settings at hand.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The selected provider's assistant name ("Claude" / "ChatGPT") for UI text.
pub fn name() -> &'static str {
    Provider::ALL.get(PROVIDER.load(Ordering::Relaxed) as usize).copied().unwrap_or_default().name()
}

pub fn sync(cfg: &Config) {
    ENABLED.store(cfg.enabled, Ordering::Relaxed);
    PROVIDER.store(cfg.provider as u8, Ordering::Relaxed);
}
