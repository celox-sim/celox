//! Source-independent hierarchy and signal metadata retained by native images.

use celox_design::{DomainKind, PortTypeKind, StateAddr};
use serde::{Deserialize, Serialize};

use crate::SignalRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReflectionScopeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReflectionSignalId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalDirection {
    Input,
    Output,
    Inout,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionScope {
    pub name: String,
    pub full_name: String,
    pub module_name: String,
    pub parent: Option<ReflectionScopeId>,
    pub children: Vec<ReflectionScopeId>,
    pub signals: Vec<ReflectionSignalId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionSignal {
    pub name: String,
    pub full_name: String,
    pub parent: ReflectionScopeId,
    pub state_address: StateAddr,
    pub signal: SignalRef,
    pub direction: SignalDirection,
    pub domain_kind: DomainKind,
    pub signed: bool,
    pub packed_dims: Vec<usize>,
    pub unpacked_dims: Vec<usize>,
    pub type_kind: PortTypeKind,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignReflection {
    scopes: Vec<ReflectionScope>,
    signals: Vec<ReflectionSignal>,
}

impl DesignReflection {
    pub fn new(scopes: Vec<ReflectionScope>, signals: Vec<ReflectionSignal>) -> Self {
        Self { scopes, signals }
    }

    pub fn scopes(&self) -> &[ReflectionScope] {
        &self.scopes
    }

    pub fn signals(&self) -> &[ReflectionSignal] {
        &self.signals
    }

    pub fn scope(&self, id: ReflectionScopeId) -> Option<&ReflectionScope> {
        self.scopes.get(id.0 as usize)
    }

    pub fn signal(&self, id: ReflectionSignalId) -> Option<&ReflectionSignal> {
        self.signals.get(id.0 as usize)
    }

    pub fn scope_by_name(&self, full_name: &str) -> Option<(ReflectionScopeId, &ReflectionScope)> {
        self.scopes
            .binary_search_by(|scope| scope.full_name.as_str().cmp(full_name))
            .ok()
            .map(|index| (ReflectionScopeId(index as u32), &self.scopes[index]))
    }

    pub fn signal_by_name(
        &self,
        full_name: &str,
    ) -> Option<(ReflectionSignalId, &ReflectionSignal)> {
        self.signals
            .binary_search_by(|signal| signal.full_name.as_str().cmp(full_name))
            .ok()
            .map(|index| (ReflectionSignalId(index as u32), &self.signals[index]))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.scopes.is_empty() {
            return Err("reflection has no root scope".into());
        }
        if self.scopes[0].parent.is_some() {
            return Err("reflection root scope has a parent".into());
        }
        for (index, scope) in self.scopes.iter().enumerate() {
            if index != 0 && scope.parent.and_then(|parent| self.scope(parent)).is_none() {
                return Err(format!("scope `{}` has an invalid parent", scope.full_name));
            }
            if index > 0 && self.scopes[index - 1].full_name >= scope.full_name {
                return Err("reflection scopes are not uniquely name-sorted".into());
            }
            if scope
                .children
                .iter()
                .any(|child| self.scope(*child).is_none())
            {
                return Err(format!("scope `{}` has an invalid child", scope.full_name));
            }
            if scope
                .signals
                .iter()
                .any(|signal| self.signal(*signal).is_none())
            {
                return Err(format!("scope `{}` has an invalid signal", scope.full_name));
            }
        }
        for (index, signal) in self.signals.iter().enumerate() {
            if self.scope(signal.parent).is_none() {
                return Err(format!(
                    "signal `{}` has an invalid parent",
                    signal.full_name
                ));
            }
            if index > 0 && self.signals[index - 1].full_name >= signal.full_name {
                return Err("reflection signals are not uniquely name-sorted".into());
            }
        }
        Ok(())
    }
}
