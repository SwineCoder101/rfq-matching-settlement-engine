use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use crate::domain::{ContractId, Oracle, OracleOutcome};

/// Scriptable oracle. Contracts with no entry are "unavailable / delayed".
#[derive(Debug, Default)]
pub struct MockOracle {
    outcomes: Mutex<HashMap<ContractId, OracleOutcome>>,
}

impl MockOracle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, contract: ContractId, outcome: OracleOutcome) {
        self.outcomes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(contract, outcome);
    }

    /// Make the contract unavailable again.
    pub fn clear(&self, contract: &ContractId) {
        self.outcomes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(contract);
    }
}

impl Oracle for MockOracle {
    fn outcome(&self, contract: &ContractId) -> Option<OracleOutcome> {
        self.outcomes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(contract)
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_contract_is_unavailable() {
        let oracle = MockOracle::new();
        let c = ContractId::new("C").unwrap();
        assert_eq!(oracle.outcome(&c), None);
        oracle.set(c.clone(), OracleOutcome::Yes);
        assert_eq!(oracle.outcome(&c), Some(OracleOutcome::Yes));
        oracle.clear(&c);
        assert_eq!(oracle.outcome(&c), None);
    }
}
