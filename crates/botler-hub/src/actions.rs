//! Actions: approval-gated jobs (`lifetime = "action"`), the only runnables that may use
//! hub-class secrets.
//!
//! `invoke_action(name, args)` checks the arguments against the action's `args` schema and opens
//! an approval request showing them; the proposal is bound to the hash of the arguments. On
//! approval the hub runs the job once with exactly those arguments (`/botler/args.json`), and its
//! outcome reaches the invoking chat as `action.result`. A declined request says so the same way.

use std::sync::Arc;

use botler_core::Source;
use botler_core::defs::Lifetime;
use botler_core::payloads::{ActionResultPayload, BuiltinPayload};
use botler_store::{
    DialogKind, NewDialog, NewProposal, NewRun, Priority, Proposal, ProposalDecision, ProposalKind, ProposalStatus,
    Run, RunKind,
};
use serde_json::Value;

use crate::hub::{Hub, HubError, Result, builtin};
use crate::ops::OpError;

/// Where the approved arguments appear in the action's container.
pub const ARGS_FILE: &str = "/botler/args.json";
/// Action runs record their proposal as `proposal:<id>` in the run's trigger.
const TRIGGER_PREFIX: &str = "proposal:";

/// The hash an approval is bound to.
pub fn args_hash(args: &Value) -> String {
    use sha2::Digest;
    // serde_json objects are sorted maps, so equal arguments serialize identically.
    hex::encode(sha2::Sha256::digest(args.to_string()))
}

/// Ask the user to approve running action `name` with `args` for `instance`.
pub async fn invoke(hub: &Hub, instance: &str, name: &str, args: Value) -> std::result::Result<Proposal, OpError> {
    let applied = hub.applied().await;
    let def = match applied.defs.jobs.get(name) {
        Some(j) if j.spec.lifetime == Lifetime::Action => j,
        _ => {
            return Err(OpError::BadRequest(format!("no action {name} (actions are jobs with lifetime = \"action\")")));
        }
    };
    let schema = def.spec.args.clone().unwrap_or(Value::Null);
    let validator = botler_core::event_check::compile(&schema)
        .map_err(|e| OpError::BadRequest(format!("action {name} has an invalid args schema: {e}")))?;
    let problems: Vec<String> =
        validator.iter_errors(&args).map(|e| format!("- {e} (at `{}`)", e.instance_path())).collect();
    if !problems.is_empty() {
        return Err(OpError::BadRequest(format!(
            "arguments of {name} do not match its schema:\n{}",
            problems.join("\n")
        )));
    }
    let pretty = serde_json::to_string_pretty(&args).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    let p = hub
        .store
        .create_proposal(
            ProposalKind::Action,
            ProposalStatus::AwaitingUser,
            &NewProposal {
                instance: instance.into(),
                title: format!("Run action `{name}`"),
                action: Some(name.into()),
                args: Some(args.clone()),
                args_hash: Some(args_hash(&args)),
                ..Default::default()
            },
        )
        .await?;
    let who = hub.store.instance(instance).await?.title.unwrap_or_else(|| instance.into());
    let secrets = if def.spec.env.secrets.is_empty() {
        "none".to_string()
    } else {
        def.spec.env.secrets.iter().map(|g| format!("`{g}`")).collect::<Vec<_>>().join(", ")
    };
    let d = hub
        .store
        .create_dialog(&NewDialog {
            instance: instance.into(),
            kind: DialogKind::Approval,
            question: format!("Run action `{name}` for {who}?"),
            detail: Some(format!(
                "{}\n\n**Arguments** (it runs with exactly these)\n```json\n{pretty}\n```\n\n**Secret groups it uses:** {secrets}",
                if def.spec.description.is_empty() { "(no description)" } else { &def.spec.description },
            )),
            ctas: vec!["approve".into(), "reject".into()],
            about: vec![],
            priority: Priority::Blocking,
            proposal_id: Some(p.id.clone()),
            spec: None,
            expires_at: None,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    Ok(p)
}

/// The user decided on action request `p`: run it, or tell the chat it was declined.
pub async fn on_approval(hub: &Arc<Hub>, p: &Proposal, approved: bool, reason: Option<&str>) -> Result<()> {
    let user = crate::transactions::USER;
    if p.status != ProposalStatus::AwaitingUser {
        return Err(HubError::Forbidden(format!("action request {} is already decided", p.id)));
    }
    let action = p.action.clone().ok_or_else(|| HubError::Forbidden(format!("proposal {} names no action", p.id)))?;
    if !approved {
        let reason = reason.ok_or_else(|| HubError::Forbidden("say why you decline it: the agent reads it".into()))?;
        let d = ProposalDecision { by: user, why: "declined by you", reason: Some(reason) };
        if !hub.store.decide_proposal(&p.id, Some(ProposalStatus::Rejected), &d).await? {
            return Err(HubError::Forbidden(format!("action request {} is already decided", p.id)));
        }
        crate::transactions::close_approval(hub, &p.id, "reject").await?;
        return result(hub, p, &action, false, serde_json::json!({ "declined": reason })).await;
    }
    let args = p.args.clone().unwrap_or(Value::Null);
    if p.args_hash.as_deref() != Some(args_hash(&args).as_str()) {
        return Err(HubError::Forbidden(format!("the arguments of action request {} changed after it was made", p.id)));
    }
    let d = ProposalDecision { by: user, why: "approved by you", reason };
    if !hub.store.decide_proposal(&p.id, Some(ProposalStatus::Approved), &d).await? {
        return Err(HubError::Forbidden(format!("action request {} is already decided", p.id)));
    }
    crate::transactions::close_approval(hub, &p.id, "approve").await?;
    let run = hub
        .store
        .create_run(&NewRun {
            runnable: action.clone(),
            kind: RunKind::Action,
            trigger_id: Some(format!("{TRIGGER_PREFIX}{}", p.id)),
            event_id: None,
            commit_id: Some(hub.applied().await.commit.clone()),
        })
        .await?;
    tracing::info!(action, proposal = %p.id, run = %run.id, "action approved; running");
    if let Err(e) = crate::jobs::try_launch(hub, &run, None).await {
        hub.store.finish_run(&run.id, botler_store::RunStatus::Failed, None, Some(&e.to_string())).await?;
        return result(hub, p, &action, false, serde_json::json!({ "error": format!("could not start: {e}") })).await;
    }
    Ok(())
}

/// The proposal an action run carries out.
pub async fn proposal_of(hub: &Hub, run: &Run) -> Result<Proposal> {
    let id = run
        .trigger_id
        .as_deref()
        .and_then(|t| t.strip_prefix(TRIGGER_PREFIX))
        .ok_or_else(|| HubError::Forbidden(format!("action run {} has no proposal", run.id)))?;
    Ok(hub.store.proposal(id).await?)
}

/// An action run ended: report its result (the last line of output, as JSON when it is) or its
/// failure to the chat that invoked it.
pub async fn finished(hub: &Hub, run: &Run, ok: bool, logs: &str, reason: Option<&str>) -> Result<()> {
    let p = proposal_of(hub, run).await?;
    let value = if ok {
        let last = logs.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        serde_json::from_str(last).unwrap_or_else(|_| Value::String(last.into()))
    } else {
        serde_json::json!({ "error": reason.unwrap_or("failed"), "logs_tail": crate::failures::tail(logs, 40) })
    };
    result(hub, &p, &run.runnable, ok, value).await
}

async fn result(hub: &Hub, p: &Proposal, action: &str, ok: bool, value: Value) -> Result<()> {
    let payload = BuiltinPayload::ActionResult(ActionResultPayload {
        proposal_id: p.id.clone(),
        action: action.into(),
        ok,
        result: value,
    });
    hub.emit(builtin(payload, Source::Runnable(action.into()))?.to(p.instance.clone())).await?;
    Ok(())
}
