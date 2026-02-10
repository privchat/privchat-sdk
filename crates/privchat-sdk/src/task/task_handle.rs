use std::sync::Weak;

use super::task_registry::TaskRegistryInner;

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct TaskHandle {
    id: u64,
    registry: Weak<TaskRegistryInner>,
}

impl TaskHandle {
    pub(crate) fn new(id: u64, registry: Weak<TaskRegistryInner>) -> Self {
        Self { id, registry }
    }

    #[allow(dead_code)]
    pub fn id(&self) -> u64 {
        self.id
    }

    #[allow(dead_code)]
    pub fn cancel(&self) -> bool {
        let Some(registry) = self.registry.upgrade() else {
            return false;
        };
        registry.cancel(self.id)
    }
}
