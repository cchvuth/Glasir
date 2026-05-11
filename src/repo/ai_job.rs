//! Claude integration for a tab: starting jobs and applying their results.

use super::action::Msg;
use super::op::Op;
use super::RepoTab;
use crate::ai;

impl RepoTab {
    pub(super) fn start_ai(&mut self, ctx: &egui::Context, p: ai::Purpose) {
        let draft = match &p {
            ai::Purpose::CommitMessage => {
                self.ai_draft = Some((self.summary.clone(), self.body.clone()));
                format!("{}\n\n{}", self.summary, self.body).trim().to_owned()
            }
            // The editor's output, with any partial resolution, not the file on disk.
            ai::Purpose::Resolve(path) => self.merge.as_ref().filter(|m| &m.path == path).map(|m| m.output.clone()).unwrap_or_default(),
            _ => String::new(),
        };
        self.ai = Some(ai::start(ctx, self.ai_cfg.clone(), self.dir.clone(), p, draft));
    }

    pub(super) fn poll_ai(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        let Some(job) = &mut self.ai else { return };
        if !job.poll() {
            return;
        }
        if job.purpose == ai::Purpose::CommitMessage && !job.text.is_empty() {
            let (s, b) = job.text.trim().split_once('\n').unwrap_or((job.text.trim(), ""));
            self.summary = s.trim().to_owned();
            self.body = b.trim().to_owned();
        }
        // Resolve from the merge editor: the answer goes into its output pane.
        let in_editor = matches!(&job.purpose, ai::Purpose::Resolve(p) if self.merge.as_ref().is_some_and(|m| &m.path == p));
        match (&job.done, &job.purpose) {
            (Some(Ok(())), ai::Purpose::Resolve(_)) if in_editor => {
                if let Some(m) = &mut self.merge {
                    m.fill(&job.text);
                }
                self.ai = None;
            }
            (Some(Err(e)), _) => {
                msgs.push(Msg { text: format!("{}: {e}", job.purpose.title()), error: true });
                if job.purpose == ai::Purpose::CommitMessage
                    && let Some((s, b)) = self.ai_draft.take()
                {
                    (self.summary, self.body) = (s, b);
                }
                // Only the modal dialogs keep a failed job around (to show the error and Regenerate).
                if in_editor || !matches!(job.purpose, ai::Purpose::PullRequest { .. } | ai::Purpose::Resolve(_)) {
                    self.ai = None;
                }
            }
            (Some(Ok(())), ai::Purpose::StashMessage) => {
                let msg = job.text.trim().trim_matches('"').to_owned();
                self.ai = None;
                self.run(ctx, Op::new("Stash", &["stash", "push", "--include-untracked", "-m", &msg]));
            }
            (Some(Ok(())), ai::Purpose::CommitMessage) => self.ai = None,
            _ => {}
        }
    }

    pub fn ai_busy(&self) -> Option<&ai::Purpose> {
        self.ai.as_ref().filter(|j| j.running()).map(|j| &j.purpose)
    }
}
