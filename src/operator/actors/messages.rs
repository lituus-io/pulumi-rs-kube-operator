use compact_str::CompactString;

/// Priority-ordered message for the actor mailbox.
pub struct PrioritizedMessage {
    pub priority: Priority,
    pub inner: StackMessage,
}

#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Copy, Debug)]
pub enum Priority {
    Deletion = 0,    // Highest
    LockRecovery = 1,
    FailureRetry = 2,
    Normal = 3,      // Lowest
}

pub enum StackMessage {
    Reconcile { trigger: ReconcileTrigger },
    Shutdown,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ReconcileTrigger {
    StackChanged,
    WorkspaceChanged,
    UpdateCompleted,
    PrerequisiteReady,
    SourceChanged,
    Timer,
    ManualRequest,
    /// Delayed retry scheduled by the actor itself (cooldown, lock backoff, etc).
    Retry,
    /// Lock recovery: force-unlock then retry.
    LockRetry,
}

/// NameKey avoids String allocation for namespace/name pairs.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NameKey {
    pub ns: CompactString,
    pub name: CompactString,
}

impl NameKey {
    pub fn new(ns: &str, name: &str) -> Self {
        Self {
            ns: CompactString::new(ns),
            name: CompactString::new(name),
        }
    }
}

impl std::fmt::Display for NameKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.ns, self.name)
    }
}
