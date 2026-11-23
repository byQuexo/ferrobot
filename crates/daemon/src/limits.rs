//! Plan limits of the host's own sign-ins: how much of the 5-hour and weekly windows Claude Code
//! and Codex (`ChatGPT`) have used. Read from the CLIs themselves, never the account details.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use ferrobot_core::ProviderKind;
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::error::ApiError;

const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct LimitWindow {
    /// 0-100.
    pub used_percent: f64,
    /// When the window starts over: Claude Code's own words (e.g. `Nov 26 at 6:59am
    /// (Europe/Berlin)`), or an RFC 3339 time for Codex.
    pub resets: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, utoipa::ToSchema)]
pub struct PlanLimits {
    pub five_hour: Option<LimitWindow>,
    pub weekly: Option<LimitWindow>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ProviderLimits {
    pub provider_id: Uuid,
    pub name: String,
    pub kind: ProviderKind,
    #[serde(flatten)]
    pub limits: PlanLimits,
    /// Why the limits could not be read.
    pub error: Option<String>,
}

/// Recent reads: `None` is the host's Claude Code login (shared by every `claude_code`
/// provider), else a Codex provider.
pub type LimitsCache = Mutex<HashMap<Option<Uuid>, (Instant, Result<PlanLimits, String>)>>;

/// Every Claude Code provider and every Codex provider signed in with `ChatGPT` (no API key).
pub async fn all(state: &AppState) -> Result<Vec<ProviderLimits>, ApiError> {
    let mut wanted = Vec::new();
    for p in state.store.list_providers().await? {
        let source = match p.kind {
            ProviderKind::ClaudeCode => None,
            ProviderKind::Codex if chatgpt(state, p.id).await? => Some(p.id),
            _ => continue,
        };
        wanted.push((p, source));
    }
    let mut sources: Vec<Option<Uuid>> = wanted.iter().map(|(_, s)| *s).collect();
    sources.sort_unstable();
    sources.dedup();
    let reads = futures_util::future::join_all(sources.iter().map(|s| read(state, *s))).await;
    let reads: HashMap<_, _> = sources.into_iter().zip(reads).collect();
    Ok(wanted
        .into_iter()
        .map(|(p, source)| {
            let (limits, error) = match &reads[&source] {
                Ok(limits) => (limits.clone(), None),
                Err(e) => (PlanLimits::default(), Some(e.clone())),
            };
            ProviderLimits {
                provider_id: p.id,
                name: p.name,
                kind: p.kind,
                limits,
                error,
            }
        })
        .collect())
}

async fn chatgpt(state: &AppState, provider: Uuid) -> Result<bool, ApiError> {
    let secrets = &state.settings.secrets;
    Ok(secrets
        .get(&crate::proxy::secret_key(provider))
        .await?
        .is_none()
        && secrets
            .get(&crate::codex::secret_key(provider))
            .await?
            .is_some())
}

async fn read(state: &AppState, source: Option<Uuid>) -> Result<PlanLimits, String> {
    let cache = || state.limits.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, read)) = cache().get(&source)
        && at.elapsed() < CACHE_TTL
    {
        return read.clone();
    }
    let read = match source {
        None => crate::claude_code::plan_limits(state).await,
        Some(id) => crate::codex::plan_limits(state, id).await,
    }
    .and_then(|limits| {
        if limits == PlanLimits::default() {
            Err("no plan limits reported".to_owned())
        } else {
            Ok(limits)
        }
    });
    cache().insert(source, (Instant::now(), read.clone()));
    read
}
