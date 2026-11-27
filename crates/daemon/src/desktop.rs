//! The agent's desktop: an X server with a window manager, Chromium, a terminal and a file
//! manager in its computer. The packages come with the CLIs image (`clis.rs`, Settings → Computers →
//! Desktop); the programs start on demand, since a stopped computer loses them. The operator
//! watches through `/v1/agents/{id}/screen` (VNC over a streaming exec, no port is opened);
//! agents with the Desktop group act through the `desktop` tool (xdotool and scrot).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use ferrobot_core::{AgentId, ToolGroup};
use ferrobot_mcp::DesktopAction;
use ferrobot_sandbox::{ExecRequest, SandboxHandle, SandboxPath};
use rmcp::model::{CallToolResult, ContentBlock};

use crate::AppState;
use crate::error::ApiError;

/// Debian packages of the desktop, added to the CLIs image.
pub const PACKAGES: [&str; 17] = [
    "arc-theme",
    "chromium",
    "fonts-dejavu-core",
    "fonts-inter",
    "fonts-jetbrains-mono",
    "fonts-wqy-microhei",
    "lxterminal",
    "openbox",
    "procps",
    "scrot",
    "socat",
    "thunar",
    "tint2",
    "x11vnc",
    "xdotool",
    "xvfb",
    "xwallpaper",
];
pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 800;
const DISPLAY: &str = ":1";
/// Exit code of [`START`] when the computer's image has no desktop.
const NO_DESKTOP: i64 = 3;
const SHOT: &str = "/tmp/ferrobot-screen.png";

/// Writes the desktop's look once (theme, dock, terminal, wallpaper, a Chromium wrapper with the
/// flags a container needs) and starts Xvfb, the window manager and the dock unless they run.
const START: &str = include_str!("desktop-start.sh");

/// Why an agent never has both a CLI account and the Desktop group.
const ACCOUNT_CONFLICT: &str = "an agent cannot have both a CLI account and the Desktop tools, \
because commands typed into its desktop's terminal skip the checks on risky steps";
/// Why a Codex agent never has a CLI account: Codex runs its own shell, which skips those checks.
const CODEX_CONFLICT: &str = "a Codex agent cannot have a CLI account, because Codex runs \
commands itself and they skip the checks on risky steps";

async fn has_any_account(state: &AppState, agent: AgentId) -> bool {
    !crate::logins::of_agent(state, agent).await.is_empty()
}

/// Refuses the Desktop group for an agent that has a CLI account.
pub(crate) async fn check_tools(
    state: &AppState,
    agent: AgentId,
    tools: &[ToolGroup],
) -> Result<(), ApiError> {
    if tools.contains(&ToolGroup::Desktop) && has_any_account(state, agent).await {
        return Err(ApiError::BadRequest(format!(
            "remove its CLI account first: {ACCOUNT_CONFLICT}"
        )));
    }
    Ok(())
}

/// Refuses a CLI account for agents that have the Desktop group or run on Codex.
pub(crate) async fn check_account_agents(
    state: &AppState,
    agents: &[AgentId],
) -> Result<(), ApiError> {
    for id in agents {
        let agent = state.store.get_agent(*id).await?;
        if agent.tools.contains(&ToolGroup::Desktop) {
            return Err(ApiError::BadRequest(format!(
                "{} has the Desktop tools; {ACCOUNT_CONFLICT}",
                agent.name
            )));
        }
        if runs_on_codex(state, agent.provider_id).await {
            return Err(ApiError::BadRequest(format!(
                "{} runs on Codex; {CODEX_CONFLICT}",
                agent.name
            )));
        }
    }
    Ok(())
}

/// Refuses moving an agent with a CLI account onto a Codex provider.
pub(crate) async fn check_provider(
    state: &AppState,
    agent: AgentId,
    provider: Option<uuid::Uuid>,
) -> Result<(), ApiError> {
    if runs_on_codex(state, provider).await && has_any_account(state, agent).await {
        return Err(ApiError::BadRequest(format!(
            "remove its CLI account first: {CODEX_CONFLICT}"
        )));
    }
    Ok(())
}

async fn runs_on_codex(state: &AppState, provider: Option<uuid::Uuid>) -> bool {
    let Some(id) = provider else { return false };
    state
        .store
        .get_provider(id)
        .await
        .is_ok_and(|p| p.kind == ferrobot_core::ProviderKind::Codex)
}

/// Who holds the operator's control of each desktop: open screen connections in control mode.
#[derive(Debug, Default)]
pub struct Desktop {
    control: Mutex<HashMap<AgentId, usize>>,
}

/// Holds the operator's control of a desktop until dropped (the screen connection closed).
#[derive(Debug)]
pub struct Control {
    desktop: Arc<Desktop>,
    agent: AgentId,
}

impl Desktop {
    pub fn take(self: &Arc<Self>, agent: AgentId) -> Control {
        *crate::lock(&self.control).entry(agent).or_default() += 1;
        Control {
            desktop: Arc::clone(self),
            agent,
        }
    }

    pub fn controlled(&self, agent: AgentId) -> bool {
        crate::lock(&self.control)
            .get(&agent)
            .is_some_and(|n| *n > 0)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let mut control = crate::lock(&self.desktop.control);
        if let Some(n) = control.get_mut(&self.agent) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                control.remove(&self.agent);
            }
        }
    }
}

fn display(cmd: Vec<String>) -> ExecRequest {
    let mut req = ExecRequest::new(cmd);
    req.env.insert("DISPLAY".into(), DISPLAY.into());
    req
}

/// Wakes the agent's computer and starts its desktop; its handle.
pub(crate) async fn start(state: &AppState, agent: AgentId) -> Result<SandboxHandle, ApiError> {
    crate::lifecycle::ensure_awake(state, agent).await?;
    let handle = crate::lifecycle::sandbox_handle(&state.store.get_agent(agent).await?)?;
    let out = state
        .backend
        .exec(&handle, &ExecRequest::new(["/bin/sh", "-c", START]))
        .await?;
    match out.exit_code {
        0 => Ok(handle),
        NO_DESKTOP => Err(ApiError::Conflict(
            "this computer has no desktop yet: turn it on in Settings → Computers, then restart the \
             computer"
                .into(),
        )),
        _ => Err(ApiError::Upstream(format!(
            "the desktop did not start: {}",
            out.stderr.trim()
        ))),
    }
}

/// The VNC server for one screen connection, speaking on stdin and stdout. x11vnc's inetd mode
/// writes to its stdin, so socat hands it a socketpair instead of exec's one-way pipes. The
/// display goes in `DISPLAY` (socat reads `:` in an address as a separator), and x11vnc ignores
/// socat's SIGTERM at the end, so it is killed by its own log name.
pub(crate) fn vnc_command(control: bool) -> Vec<String> {
    let view_only = if control { "" } else { " -viewonly" };
    let script = format!(
        "log=/tmp/x11vnc-$$.log\n\
         /usr/bin/socat STDIO \"EXEC:/usr/bin/x11vnc -inetd -nopw -shared -quiet -o $log{view_only}\"\n\
         pkill -KILL -f \"$log\"; rm -f \"$log\""
    );
    vec![
        "/usr/bin/env".into(),
        format!("DISPLAY={DISPLAY}"),
        "/bin/sh".into(),
        "-c".into(),
        script,
    ]
}

/// xdotool's arguments for an action, after checking it stays on the screen.
fn xdotool(action: &DesktopAction) -> Result<Vec<String>, String> {
    let on_screen = |x: u32, y: u32| {
        if x < WIDTH && y < HEIGHT {
            Ok(vec![
                "mousemove".into(),
                "--sync".into(),
                x.to_string(),
                y.to_string(),
            ])
        } else {
            Err(format!("the screen is {WIDTH}×{HEIGHT}; {x},{y} is off it"))
        }
    };
    let mut args = match action {
        DesktopAction::Screenshot => return Err("not an xdotool step".into()),
        DesktopAction::Move { x, y } => on_screen(*x, *y)?,
        DesktopAction::Click {
            x,
            y,
            button,
            double,
        } => {
            let mut args = on_screen(*x, *y)?;
            args.push("click".into());
            if *double {
                args.extend(["--repeat".into(), "2".into()]);
            }
            args.push(button.to_string());
            args
        }
        DesktopAction::Scroll {
            x,
            y,
            button,
            amount,
        } => {
            let mut args = on_screen(*x, *y)?;
            args.extend([
                "click".into(),
                "--repeat".into(),
                amount.to_string(),
                button.to_string(),
            ]);
            args
        }
        DesktopAction::Type { text } => vec![
            "type".into(),
            "--delay".into(),
            "12".into(),
            "--".into(),
            text.clone(),
        ],
        DesktopAction::Key { keys } => {
            let mut args = vec!["key".into(), "--".into()];
            args.extend(keys.iter().cloned());
            args
        }
    };
    args.insert(0, "/usr/bin/xdotool".into());
    Ok(args)
}

/// Runs an agent's desktop step. `Err` carries a message for the agent.
pub(crate) async fn act(
    state: &AppState,
    agent: AgentId,
    action: DesktopAction,
) -> Result<CallToolResult, String> {
    if state.desktop.controlled(agent) {
        return Err(
            "Your operator is using your desktop right now. Do something else and try again \
             later."
                .into(),
        );
    }
    let handle = start(state, agent).await.map_err(|e| e.to_string())?;
    let failed = |e: ferrobot_sandbox::SandboxError| e.to_string();
    if action == DesktopAction::Screenshot {
        let shot = state
            .backend
            .exec(
                &handle,
                &display(vec!["/usr/bin/scrot".into(), "-o".into(), SHOT.into()]),
            )
            .await
            .map_err(failed)?;
        if shot.exit_code != 0 {
            return Err(format!("no screenshot: {}", shot.stderr.trim()));
        }
        let path = SandboxPath::parse(SHOT).map_err(|e| e.to_string())?;
        let png = state
            .backend
            .read_file(&handle, &path)
            .await
            .map_err(failed)?;
        let data = base64::engine::general_purpose::STANDARD.encode(png);
        return Ok(CallToolResult::success(vec![ContentBlock::image(
            data,
            "image/png",
        )]));
    }
    let out = state
        .backend
        .exec(&handle, &display(xdotool(&action)?))
        .await
        .map_err(failed)?;
    if out.exit_code != 0 {
        return Err(format!("that did not work: {}", out.stderr.trim()));
    }
    Ok(CallToolResult::success(vec![ContentBlock::text(
        "Done. Take a screenshot to see the result.",
    )]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_become_xdotool_arguments_on_the_screen_only() {
        let click = DesktopAction::Click {
            x: 10,
            y: 20,
            button: 3,
            double: false,
        };
        assert_eq!(
            xdotool(&click).unwrap(),
            [
                "/usr/bin/xdotool",
                "mousemove",
                "--sync",
                "10",
                "20",
                "click",
                "3"
            ]
        );
        let typed = DesktopAction::Type {
            text: "-rf ~".into(),
        };
        assert_eq!(xdotool(&typed).unwrap()[4..], ["--", "-rf ~"]);
        let off = DesktopAction::Move { x: WIDTH, y: 0 };
        assert!(xdotool(&off).is_err());
        assert!(vnc_command(false)[4].contains(" -viewonly\""));
        assert!(!vnc_command(true)[4].contains("-viewonly"));
    }

    #[test]
    fn control_lasts_while_a_connection_holds_it() {
        let desktop = Arc::new(Desktop::default());
        let agent = AgentId::new();
        let first = desktop.take(agent);
        let second = desktop.take(agent);
        drop(first);
        assert!(desktop.controlled(agent));
        drop(second);
        assert!(!desktop.controlled(agent));
    }

    /// `sh` in the computer stops at a CR (`unset: : bad variable name`), and a Windows checkout
    /// gives one unless `.gitattributes` keeps the script LF.
    #[test]
    fn the_start_script_has_unix_line_endings() {
        assert!(
            !START.contains('\r'),
            "desktop-start.sh has CRLF line endings"
        );
    }
}
