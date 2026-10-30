//! What an agent is doing, as live viewers see it: the steps of waking, and its runtime status.

use chrono::{DateTime, Utc};
use serde::Serialize;
use ts_rs::TS;

/// A step of waking an agent, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum WakeStep {
    /// Building (or pulling) the agent's image.
    BuildingImage,
    PreparingWorkspace,
    StartingContainer,
}

impl WakeStep {
    pub fn describe(self) -> &'static str {
        match self {
            Self::BuildingImage => "building image",
            Self::PreparingWorkspace => "preparing workspace",
            Self::StartingContainer => "starting container",
        }
    }
}

/// Where waking an instance stands: in progress, or failed (until the next attempt).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum WakePhase {
    Waking {
        step: WakeStep,
    },
    Failed {
        error: String,
    },
    /// Its last turn failed with a transient model error; the hub tries again at `at`.
    Retrying {
        at: DateTime<Utc>,
        error: String,
    },
}

/// An agent's runtime status, broadcast whenever it changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentStatus {
    Waking {
        step: WakeStep,
    },
    Running,
    /// Asleep: woken by its next delivery.
    Dormant,
    /// Waking failed.
    Failed {
        error: String,
    },
    /// A breaker paused it (provider errors, budget, wake churn).
    Paused {
        reason: String,
    },
    /// Its last turn failed with a transient model error; the hub tries again at `at`.
    Retrying {
        at: DateTime<Utc>,
        error: String,
    },
}

impl From<WakePhase> for AgentStatus {
    fn from(p: WakePhase) -> Self {
        match p {
            WakePhase::Waking { step } => Self::Waking { step },
            WakePhase::Failed { error } => Self::Failed { error },
            WakePhase::Retrying { at, error } => Self::Retrying { at, error },
        }
    }
}
