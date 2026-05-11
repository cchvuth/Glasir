//! Claude-powered helpers using the user's own token: commit messages, commit
//! explanations, pull-request descriptions, stash messages and conflict
//! resolution. `client` talks to the API; `tasks` builds the prompts and runs jobs.

mod client;
mod tasks;

pub use client::{Config, MODELS, remove_plain_token, save_plain_token, save_token, token_source};
pub use tasks::{Job, Purpose, start, test};
