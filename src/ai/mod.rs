//! Claude-powered helpers using the user's own token: commit messages, commit
//! explanations, pull-request descriptions, stash messages and conflict
//! resolution, explaining working changes and planning commits (Commit Composer).
//! `client` talks to the API; `tasks` builds the prompts and runs jobs.

mod client;
mod tasks;

pub use client::{Config, IN_FILE, IN_KEYCHAIN, MODELS, env_source, find_saved, remove_plain_token, save_plain_token, save_token, tls};
pub use tasks::{Job, Purpose, start, test};

use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Settings → "Enable AI features", mirrored every frame for UI code without the settings at hand.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}
