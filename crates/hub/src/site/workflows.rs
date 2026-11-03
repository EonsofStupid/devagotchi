//! `content/workflows.json`: the step-by-step setups an AI can start for its person and hand
//! them as one link (`src/workflows.rs`), and `content/messages.json` `workflow`, the words
//! of the page that link opens (`web/workflow.rs`).
//!
//! Workflows are content and steps are code: a workflow is an id, a title, an intro and an
//! ordered list of steps, each with its own words and a `kind` the hub implements
//! ([`KINDS`]). `check-site` refuses an unknown kind, a field nobody reads and a workflow
//! that could not run (no steps, a closing step that is not last, a machine OS the hub
//! cannot install on). A workflow that cannot be done yet says so in `unavailable` and has
//! no steps: the hub offers it with that reason and never starts it, so nobody is handed a
//! command that cannot work.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Ctx, Site};

/// The step kinds this hub implements.
pub const KINDS: [&str; 4] = [SECURE_ACCOUNT, MACHINE, PHONE, DONE];
/// Done when the account is secured (a passkey or a linked sign-in). Optional: offered once
/// a step worked, it never holds a workflow up.
pub const SECURE_ACCOUNT: &str = "secure_account";
/// Done when a machine enrolled through this workflow is online. `os` is one of
/// [`MACHINE_OS`], or absent: the person picks on the page.
pub const MACHINE: &str = "machine";
/// Done when the account has a phone (a push subscription).
pub const PHONE: &str = "phone";
/// The closing step: done when every step before it is.
pub const DONE: &str = "done";
/// What a machine step installs on: the hub's one-line joins.
pub const MACHINE_OS: [&str; 4] = ["linux", "windows", "docker", "host"];

/// The longest workflow id, and the most steps and workflows a site has.
const ID_MAX: usize = 40;
const STEPS_MAX: usize = 12;
const WORKFLOWS_MAX: usize = 64;

/// `content/workflows.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowsFile {
    pub workflows: Vec<Workflow>,
}

/// One workflow, as the site says it. Kept whole with a workflow when it starts
/// (`tenancy_workflows.definition`), so a site changed later never reshapes one under way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    /// `[a-z0-9-]`, what `workflow_start` takes.
    pub id: String,
    /// Plain text.
    pub title: String,
    /// Inline markup.
    pub intro: String,
    /// Why it cannot be done yet (inline markup). Such a workflow has no steps and is never
    /// started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(default)]
    pub steps: Vec<StepDef>,
}

/// One step: its words and the kind the hub runs it as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDef {
    pub kind: String,
    /// Plain text.
    pub title: String,
    /// Inline markup.
    pub text: String,
    /// A machine step's OS ([`MACHINE_OS`]); absent, the person picks on the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

impl WorkflowsFile {
    /// The workflow called `id`.
    pub fn get(&self, id: &str) -> Option<&Workflow> {
        self.workflows.iter().find(|workflow| workflow.id == id)
    }

    /// Every rule a workflow must follow, with where it broke.
    pub fn check(&self) -> Result<(), String> {
        let what = |text: String| format!("content/workflows.json {text}");
        if self.workflows.len() > WORKFLOWS_MAX {
            return Err(what(format!("holds at most {WORKFLOWS_MAX} workflows")));
        }
        let mut seen = std::collections::BTreeSet::new();
        for workflow in &self.workflows {
            let id = &workflow.id;
            if id.is_empty()
                || id.len() > ID_MAX
                || !id.starts_with(|c: char| c.is_ascii_lowercase())
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(what(format!(
                    "{id:?}: an id is a letter then [a-z0-9-], at most {ID_MAX}"
                )));
            }
            if !seen.insert(id.as_str()) {
                return Err(what(format!("{id}: two workflows have this id")));
            }
            if workflow.title.trim().is_empty() {
                return Err(what(format!("{id}: title is empty")));
            }
            workflow.check_steps().map_err(|why| what(format!("{id}: {why}")))?;
        }
        Ok(())
    }
}

impl Workflow {
    /// Whether the hub can start it.
    pub fn available(&self) -> bool {
        self.unavailable.is_none()
    }

    fn check_steps(&self) -> Result<(), String> {
        if self.unavailable.is_some() {
            if !self.steps.is_empty() {
                return Err("a workflow marked unavailable has no steps".into());
            }
            return Ok(());
        }
        if self.steps.is_empty() {
            return Err("steps: a workflow has at least one step (or says why it is unavailable)".into());
        }
        if self.steps.len() > STEPS_MAX {
            return Err(format!("steps: at most {STEPS_MAX}"));
        }
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, step) in self.steps.iter().enumerate() {
            let at = format!("steps[{index}]");
            if !KINDS.contains(&step.kind.as_str()) {
                return Err(format!(
                    "{at}: kind {:?} is not one this hub runs ({})",
                    step.kind,
                    KINDS.join(", ")
                ));
            }
            if step.title.trim().is_empty() {
                return Err(format!("{at}: title is empty"));
            }
            match (step.kind.as_str(), step.os.as_deref()) {
                (MACHINE, Some(os)) if !MACHINE_OS.contains(&os) => {
                    return Err(format!(
                        "{at}: os {os:?} is not one the hub installs on ({})",
                        MACHINE_OS.join(", ")
                    ));
                }
                (MACHINE, _) | (_, None) => {}
                (kind, Some(_)) => return Err(format!("{at}: a {kind} step takes no os")),
            }
            *counts.entry(step.kind.as_str()).or_default() += 1;
        }
        for kind in [SECURE_ACCOUNT, PHONE, DONE] {
            if counts.get(kind).copied().unwrap_or(0) > 1 {
                return Err(format!("steps: at most one {kind} step"));
            }
        }
        if self.steps.iter().rposition(|step| step.kind == DONE).is_some_and(|at| at + 1 != self.steps.len()) {
            return Err("steps: the done step closes a workflow, so it is the last".into());
        }
        if self.steps.iter().all(|step| step.kind == DONE) {
            return Err("steps: a workflow does something before it is done".into());
        }
        Ok(())
    }
}

/// The words of the workflow page (`content/messages.json` `workflow`). Plain text unless it
/// says inline markup. Facts are what the page and `workflow_status` say a step stands at.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCopy {
    /// `{n}` and `{total}`.
    pub step_of: String,
    pub state_done: String,
    pub state_current: String,
    pub state_pending: String,
    pub state_unavailable: String,
    /// An optional step not done yet (keeping the account).
    pub state_optional: String,
    pub dead_title: String,
    /// Inline markup.
    pub dead_text: String,
    /// Inline markup: a finished workflow, to whoever follows its link.
    pub finished: String,
    pub check_again: String,
    pub copy: String,
    pub copied: String,
    pub get_command: String,
    pub make_passkey: String,
    /// Keeping the account, offered once a step worked, to the browser that did it.
    pub keep_title: String,
    /// Inline markup: why, and that it is optional.
    pub keep_text: String,
    /// Inline markup: kept.
    pub keep_done: String,
    /// Inline markup: how long the line works.
    pub command_note: String,
    pub name_label: String,
    pub os_label: String,
    /// What each OS is called (keys: [`MACHINE_OS`]).
    pub os: BTreeMap<String, String>,
    /// Where to paste each OS's line (inline markup; keys: [`MACHINE_OS`]).
    pub os_hint: BTreeMap<String, String>,
    /// The name a machine gets unless the AI or the person gives one (keys: [`MACHINE_OS`]).
    pub default_names: BTreeMap<String, String>,
    /// Inline markup: why there is no Mac to pick.
    pub mac: String,
    /// Inline markup: a hub that hands out no builds has no one-line install.
    pub no_downloads: String,
    /// Inline markup; `{os}`: a hub without a build for that OS.
    pub no_build_for: String,
    /// Inline markup: a hub that sends no notifications has no phone step.
    pub no_push: String,
    pub notify: String,
    /// Inline markup: open the same link on the phone.
    pub phone_hint: String,
    /// Inline markup: a post from another site, refused.
    pub refused: String,
    pub facts: FactsCopy,
}

/// What a step stands at, in words (plain; `{machine}`, `{n}`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactsCopy {
    pub secured: String,
    pub not_secured: String,
    pub machine_none: String,
    pub machine_waiting: String,
    pub machine_offline: String,
    pub machine_online: String,
    pub phone_on: String,
    pub phone_off: String,
    pub done: String,
}

/// What one message is checked with: where, its text, its placeholders, inline or plain.
type Check<'a> = (String, &'a str, Vec<(&'static str, &'static str)>, bool);

impl Site {
    /// Every workflow's words and the page's, each with what fills it where it is drawn.
    pub(super) fn workflow_checks(&self) -> Result<Vec<Check<'_>>, String> {
        self.workflows.check()?;
        let copy = &self.messages.workflow;
        for (what, map) in [
            ("os", &copy.os),
            ("os_hint", &copy.os_hint),
            ("default_names", &copy.default_names),
        ] {
            let keys: Vec<&str> = map.keys().map(String::as_str).collect();
            let mut wanted = MACHINE_OS.to_vec();
            wanted.sort_unstable();
            if keys != wanted {
                return Err(format!(
                    "content/messages.json workflow.{what}: one entry for each of {}",
                    MACHINE_OS.join(", ")
                ));
            }
        }
        for (os, name) in &copy.default_names {
            if !crate::config::valid_name(name) || name.len() > crate::tenant_machines::HOSTED_NAME_MAX {
                return Err(format!(
                    "content/messages.json workflow.default_names.{os}: {name:?} is not a machine name (lowercase letters, digits and dashes, at most {})",
                    crate::tenant_machines::HOSTED_NAME_MAX
                ));
            }
        }
        let machine = ("machine", "nas");
        let mut checks: Vec<Check<'_>> = vec![
            ("step_of".into(), &copy.step_of, vec![("n", "1"), ("total", "3")], false),
            ("state_done".into(), &copy.state_done, vec![], false),
            ("state_current".into(), &copy.state_current, vec![], false),
            ("state_pending".into(), &copy.state_pending, vec![], false),
            ("state_unavailable".into(), &copy.state_unavailable, vec![], false),
            ("state_optional".into(), &copy.state_optional, vec![], false),
            ("dead_title".into(), &copy.dead_title, vec![], false),
            ("dead_text".into(), &copy.dead_text, vec![], true),
            ("finished".into(), &copy.finished, vec![], true),
            ("check_again".into(), &copy.check_again, vec![], false),
            ("copy".into(), &copy.copy, vec![], false),
            ("copied".into(), &copy.copied, vec![], false),
            ("get_command".into(), &copy.get_command, vec![], false),
            ("make_passkey".into(), &copy.make_passkey, vec![], false),
            ("keep_title".into(), &copy.keep_title, vec![], false),
            ("keep_text".into(), &copy.keep_text, vec![], true),
            ("keep_done".into(), &copy.keep_done, vec![], true),
            ("command_note".into(), &copy.command_note, vec![], true),
            ("name_label".into(), &copy.name_label, vec![], false),
            ("os_label".into(), &copy.os_label, vec![], false),
            ("mac".into(), &copy.mac, vec![], true),
            ("no_downloads".into(), &copy.no_downloads, vec![], true),
            ("no_build_for".into(), &copy.no_build_for, vec![("os", "Windows")], true),
            ("no_push".into(), &copy.no_push, vec![], true),
            ("notify".into(), &copy.notify, vec![], false),
            ("phone_hint".into(), &copy.phone_hint, vec![], true),
            ("refused".into(), &copy.refused, vec![], true),
            ("facts.secured".into(), &copy.facts.secured, vec![], false),
            ("facts.not_secured".into(), &copy.facts.not_secured, vec![], false),
            ("facts.machine_none".into(), &copy.facts.machine_none, vec![], false),
            ("facts.machine_waiting".into(), &copy.facts.machine_waiting, vec![machine], false),
            ("facts.machine_offline".into(), &copy.facts.machine_offline, vec![machine], false),
            ("facts.machine_online".into(), &copy.facts.machine_online, vec![machine], false),
            ("facts.phone_on".into(), &copy.facts.phone_on, vec![("n", "1")], false),
            ("facts.phone_off".into(), &copy.facts.phone_off, vec![], false),
            ("facts.done".into(), &copy.facts.done, vec![], false),
        ];
        for (os, text) in &copy.os {
            checks.push((format!("os.{os}"), text, vec![], false));
        }
        for (os, text) in &copy.os_hint {
            checks.push((format!("os_hint.{os}"), text, vec![], true));
        }
        let mut out: Vec<Check<'_>> = checks
            .into_iter()
            .map(|(what, text, vars, inline)| (format!("content/messages.json workflow.{what}"), text, vars, inline))
            .collect();
        for workflow in &self.workflows.workflows {
            let at = |what: &str| format!("content/workflows.json {}: {what}", workflow.id);
            out.push((at("title"), &workflow.title, vec![], false));
            out.push((at("intro"), &workflow.intro, vec![], true));
            if let Some(why) = &workflow.unavailable {
                out.push((at("unavailable"), why, vec![], true));
            }
            for (index, step) in workflow.steps.iter().enumerate() {
                out.push((at(&format!("steps[{index}].title")), &step.title, vec![], false));
                out.push((at(&format!("steps[{index}].text")), &step.text, vec![], true));
            }
        }
        Ok(out)
    }

    /// `text` from the site, filled and drawn as inline markup.
    pub fn inline_text(&self, text: &str) -> String {
        Ctx::new(self).inline(text)
    }
}
