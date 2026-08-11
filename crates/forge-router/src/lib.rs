//! Routing boundary without a routing algorithm.
//!
//! Phase 4A resolves eligible candidate configurations and retrieves a stable,
//! policy-filtered evidence snapshot. It deliberately has no method that
//! selects an agent.

#![deny(rust_2018_idioms)]

use std::collections::BTreeSet;

use forge_core::agent::{AdapterStatus, AgentConfig, AgentDescriptor, Capability};
use forge_core::ids::AgentId;
use forge_core::routing::{
    CandidateAgent, CandidateAgentSet, RoutingContractError, RoutingEvidence, RoutingRequest,
};
use forge_store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateAvailability {
    Available,
    Unavailable { reason: String },
}

/// Caller-supplied current configuration and availability. Availability is an
/// explicit probe result; the router never guesses from an agent name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRequest {
    pub config: AgentConfig,
    pub availability: CandidateAvailability,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateRequirements {
    pub capabilities: BTreeSet<Capability>,
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("candidate agent `{0}` is not registered")]
    UnregisteredAgent(AgentId),
    #[error("candidate agent `{agent_id}` is unavailable: {reason}")]
    UnavailableAgent { agent_id: AgentId, reason: String },
    #[error("candidate agent `{agent_id}` lacks required capability `{capability:?}`")]
    IneligibleAgent {
        agent_id: AgentId,
        capability: Capability,
    },
    #[error(transparent)]
    Contract(#[from] RoutingContractError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Resolves registered, implemented, currently available candidates without
/// hard-coding any provider identity.
pub fn resolve_candidates(
    registry: &[AgentDescriptor],
    requested: Vec<CandidateRequest>,
    requirements: &CandidateRequirements,
) -> Result<CandidateAgentSet, RouterError> {
    let mut candidates = Vec::with_capacity(requested.len());
    for request in requested {
        let agent_id = request.config.agent_id.clone();
        let descriptor = registry
            .iter()
            .find(|descriptor| descriptor.agent_id == agent_id)
            .ok_or_else(|| RouterError::UnregisteredAgent(agent_id.clone()))?;
        if descriptor.adapter_status != AdapterStatus::Implemented {
            return Err(RouterError::UnavailableAgent {
                agent_id,
                reason: "adapter is not implemented".into(),
            });
        }
        if let CandidateAvailability::Unavailable { reason } = request.availability {
            return Err(RouterError::UnavailableAgent { agent_id, reason });
        }
        for capability in &requirements.capabilities {
            if !descriptor.capabilities.contains(capability) {
                return Err(RouterError::IneligibleAgent {
                    agent_id,
                    capability: capability.clone(),
                });
            }
        }
        candidates.push(CandidateAgent::new(agent_id, request.config)?);
    }
    CandidateAgentSet::new(candidates).map_err(Into::into)
}

/// Read-only façade over the store's routing query. No agent selection or
/// execution exists at this boundary in Phase 4A.
#[derive(Debug, Clone)]
pub struct RoutingContract {
    store: Store,
}

impl RoutingContract {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    pub async fn evidence(&self, request: &RoutingRequest) -> Result<RoutingEvidence, RouterError> {
        Ok(self.store.routing_evidence(request).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(
        id: &str,
        status: AdapterStatus,
        capabilities: Vec<Capability>,
    ) -> AgentDescriptor {
        AgentDescriptor {
            agent_id: AgentId::new(id).unwrap(),
            display_name: id.into(),
            harness: format!("{id}-harness"),
            executable: None,
            default_model: None,
            capabilities,
            adapter_status: status,
        }
    }

    fn request(id: &str, availability: CandidateAvailability) -> CandidateRequest {
        CandidateRequest {
            config: AgentConfig::new(AgentId::new(id).unwrap(), format!("{id}-harness")),
            availability,
        }
    }

    #[test]
    fn candidates_are_provider_agnostic_and_capability_checked() {
        let registry = vec![
            descriptor(
                "local-specialist",
                AdapterStatus::Implemented,
                vec![Capability::EditFiles, Capability::RunCommands],
            ),
            descriptor(
                "remote-worker",
                AdapterStatus::Implemented,
                vec![Capability::EditFiles, Capability::RunCommands],
            ),
        ];
        let requirements = CandidateRequirements {
            capabilities: BTreeSet::from([Capability::EditFiles, Capability::RunCommands]),
        };
        let candidates = resolve_candidates(
            &registry,
            vec![
                request("remote-worker", CandidateAvailability::Available),
                request("local-specialist", CandidateAvailability::Available),
            ],
            &requirements,
        )
        .unwrap();
        assert_eq!(
            candidates
                .agent_ids()
                .map(AgentId::as_str)
                .collect::<Vec<_>>(),
            vec!["local-specialist", "remote-worker"]
        );
    }

    #[test]
    fn unregistered_and_unavailable_candidates_are_rejected() {
        let registry = vec![descriptor(
            "registered",
            AdapterStatus::Implemented,
            Vec::new(),
        )];
        assert!(matches!(
            resolve_candidates(
                &registry,
                vec![request("missing", CandidateAvailability::Available)],
                &CandidateRequirements::default(),
            ),
            Err(RouterError::UnregisteredAgent(_))
        ));
        assert!(matches!(
            resolve_candidates(
                &registry,
                vec![request(
                    "registered",
                    CandidateAvailability::Unavailable {
                        reason: "not configured".into()
                    }
                )],
                &CandidateRequirements::default(),
            ),
            Err(RouterError::UnavailableAgent { .. })
        ));
    }
}
