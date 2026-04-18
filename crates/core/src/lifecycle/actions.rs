//! Action executor — translates LifecycleAction into gh CLI commands.

use super::LifecycleAction;
use tracing::{info, warn};

/// Execute a lifecycle action via shell commands.
///
/// Returns the command output or error message.
pub async fn execute(action: &LifecycleAction) -> Result<String, String> {
    match action {
        LifecycleAction::MergePR { repo, number, method } => {
            run(&format!("gh pr merge {number} --repo {repo} --{method}")).await
        }
        LifecycleAction::RerunCI { repo, run_id } => {
            run(&format!("gh run rerun {run_id} --repo {repo} --failed")).await
        }
        LifecycleAction::AssignCopilot { repo, issue_number } => {
            // Get node ID first, then assign via GraphQL
            let node_id = run(&format!(
                "gh api graphql -f query='{{ repository(owner:\"{}\",name:\"{}\") {{ issue(number:{}) {{ id }} }} }}' --jq '.data.repository.issue.id'",
                repo.split('/').next().unwrap_or("plures"),
                repo.split('/').nth(1).unwrap_or(""),
                issue_number,
            )).await?;

            let bot_id = "BOT_kgDOC9w8XQ";
            run(&format!(
                "gh api graphql -f query='mutation {{ addAssigneesToAssignable(input: {{assignableId: \"{}\", assigneeIds: [\"{}\"]}}) {{ clientMutationId }} }}'",
                node_id.trim(), bot_id,
            )).await
        }
        LifecycleAction::CreateCIFeedback { repo, pr_number, error_details, is_infra } => {
            let prefix = if *is_infra { "(infra) " } else { "" };
            let labels = if *is_infra { "ci-failure" } else { "ci-failure,bug" };
            run(&format!(
                "gh issue create --repo {repo} --title '[ci-feedback] {prefix}Fix CI failures on PR #{pr_number}' --body 'CI failing on PR #{pr_number}.\\n\\n{error_details}' --label {labels}",
            )).await
        }
        LifecycleAction::CloseIssue { repo, issue_number, reason } => {
            run(&format!("gh issue close {issue_number} --repo {repo} --comment '{reason}'")).await
        }
        LifecycleAction::AddLabel { repo, number, label } => {
            run(&format!("gh issue edit {number} --repo {repo} --add-label {label}")).await
        }
        LifecycleAction::RemoveLabel { repo, number, label } => {
            run(&format!("gh issue edit {number} --repo {repo} --remove-label {label}")).await
        }
        LifecycleAction::ApprovePR { repo, number } => {
            run(&format!("gh pr review {number} --repo {repo} --approve --body 'Auto-approved: CI green + review complete.'")).await
        }
        LifecycleAction::CreateRelease { repo, tag, title, body } => {
            run(&format!("gh release create {tag} --repo {repo} --title '{title}' --notes '{body}' --latest --generate-notes")).await
        }
        LifecycleAction::Notify { message } => {
            info!(message, "lifecycle notification");
            Ok(message.clone())
        }
        LifecycleAction::Noop { reason } => {
            info!(reason, "lifecycle noop");
            Ok(reason.clone())
        }
    }
}

/// Execute a batch of actions, stopping on first error.
pub async fn execute_all(actions: &[LifecycleAction]) -> Vec<Result<String, String>> {
    let mut results = Vec::new();
    for action in actions {
        let result = execute(action).await;
        let is_err = result.is_err();
        results.push(result);
        if is_err {
            warn!("action failed, stopping batch");
            break;
        }
    }
    results
}

async fn run(cmd: &str) -> Result<String, String> {
    info!(cmd, "executing lifecycle action");
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| format!("exec error: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("exit {}: {}", output.status, stderr))
    }
}
