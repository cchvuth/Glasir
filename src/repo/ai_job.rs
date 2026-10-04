//! Claude integration for a tab: starting jobs and applying their results.

use super::action::Msg;
use super::op::Op;
use super::RepoTab;
use crate::ai;

impl RepoTab {
    pub(super) fn start_ai(&mut self, ctx: &egui::Context, p: ai::Purpose) {
        // The inline Explain panel only shows the selected commit's job: a new Explain replaces a running one.
        let explain = |p: &ai::Purpose| matches!(p, ai::Purpose::Explain(_) | ai::Purpose::ExplainRange { .. });
        if explain(&p) && self.ai_busy().is_some_and(explain) {
            self.ai = None;
        }
        if !self.ai_free() {
            return;
        }
        let draft = match &p {
            ai::Purpose::Compose => return self.start_compose(ctx, None),
            ai::Purpose::CommitMessage { .. } => {
                self.ai_draft = Some((self.summary.clone(), self.body.clone()));
                format!("{}\n\n{}", self.summary, self.body).trim().to_owned()
            }
            // The editor's output, with any partial resolution, or the file as read now:
            // applying the reply later refuses if the file changed meanwhile.
            ai::Purpose::Resolve(path) => match self.merge.as_ref().filter(|m| &m.path == path) {
                Some(m) => m.output.clone(),
                None => std::fs::read_to_string(self.dir.join(path)).unwrap_or_default(),
            },
            _ => String::new(),
        };
        self.ai = Some(ai::start(ctx, self.ai_cfg.clone(), self.dir.clone(), p, draft));
    }

    pub(super) fn poll_ai(&mut self, ctx: &egui::Context, msgs: &mut Vec<Msg>) {
        let Some(job) = &mut self.ai else { return };
        if !job.poll() {
            return;
        }
        let message = matches!(job.purpose, ai::Purpose::CommitMessage { .. });
        if message && !job.text.is_empty() {
            (self.summary, self.body) = super::message::split(&job.text);
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
                if message
                    && let Some((s, b)) = self.ai_draft.take()
                {
                    (self.summary, self.body) = (s, b);
                }
                if job.purpose == ai::Purpose::Compose {
                    self.compose = None;
                }
                // Only the modal dialogs keep a failed job around (to show the error and Regenerate).
                if in_editor || !matches!(job.purpose, ai::Purpose::PullRequest { .. } | ai::Purpose::Resolve(_)) {
                    self.ai = None;
                }
            }
            (Some(Ok(())), ai::Purpose::StashMessage) => {
                let msg = job.text.trim().trim_matches('"').to_owned();
                self.ai = None;
                if !self.ai_cfg.enabled {
                    return;
                }
                self.run(ctx, Op::new("Stash", &["stash", "push", "--include-untracked", "-m", &msg]));
            }
            (Some(Ok(())), ai::Purpose::CommitMessage { .. }) => self.ai = None,
            (Some(Ok(())), ai::Purpose::Compose) => {
                if let Some(plan) = &mut self.compose
                    && let Err(e) = plan.fill(&job.text)
                {
                    msgs.push(Msg { text: format!("Compose commits: {e}"), error: true });
                    self.compose = None;
                }
                self.ai = None;
            }
            _ => {}
        }
    }

    pub(super) fn cancel_ai(&mut self) {
        if let Some(j) = self.ai.take()
            && let Some((s, b)) = self.ai_draft.take()
            && matches!(j.purpose, ai::Purpose::CommitMessage { .. })
        {
            (self.summary, self.body) = (s, b);
        }
    }

    /// AI is on and idle; else says what it's still doing.
    pub(super) fn ai_free(&mut self) -> bool {
        if !self.ai_cfg.enabled {
            return false;
        }
        // A replaced job is dropped mid-stream: its partial text stays and its follow-up (draft restore, stash) never runs.
        if let Some(busy) = self.ai_busy().map(|p| p.title()).or(self.compose_task.is_some().then(|| "Compose commits".into())) {
            self.notes.push(Msg { text: format!("{} is still working on {busy}; wait for it to finish", ai::name()), error: true });
            return false;
        }
        true
    }

    pub fn ai_busy(&self) -> Option<&ai::Purpose> {
        self.ai.as_ref().filter(|j| j.running()).map(|j| &j.purpose)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::super::{Action, RepoTab, Task, View};
    use crate::ai;

    #[test]
    fn busy_job_is_not_replaced() {
        let dir = scratch("ai-busy");
        sh(&dir, &["init", "-q"]);
        let ctx = egui::Context::default();
        let mut t = RepoTab::new(&ctx, dir.clone(), 100, View::default());
        t.ai_cfg.enabled = true;
        let (_tx, rx) = std::sync::mpsc::channel();
        t.compose_task = Some(Task(rx));
        t.apply(&ctx, Action::Ai(ai::Purpose::StashMessage));
        assert!(t.ai.is_none());
        assert!(t.notes.iter().any(|m| m.text.contains("Compose commits")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
