use super::manager::ResourceManager;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// RAII guard for allocated resources
/// Resources are automatically released when this guard is dropped
pub struct ResourceGuard {
    job_id: Uuid,
    resource_manager: Arc<RwLock<ResourceManager>>,
    released: bool,
}

impl ResourceGuard {
    pub fn new(job_id: Uuid, resource_manager: Arc<RwLock<ResourceManager>>) -> Self {
        Self {
            job_id,
            resource_manager,
            released: false,
        }
    }

    /// Manually release resources (useful for explicit teardown)
    pub async fn release(mut self) {
        if !self.released {
            let manager = self.resource_manager.write().await;
            let _ = manager.release_resources(self.job_id).await;
            self.released = true;
        }
    }
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        if !self.released {
            // We can't use async in Drop, so we spawn a task
            // This ensures resources are eventually released even if not explicitly
            let job_id = self.job_id;
            let resource_manager = Arc::clone(&self.resource_manager);

            tokio::spawn(async move {
                let manager = resource_manager.write().await;
                let _ = manager.release_resources(job_id).await;
                log::debug!("Resources released for job {} via Drop", job_id);
            });
        }
    }
}

/// Extension trait for ResourceManager to create guards
#[allow(async_fn_in_trait)]
pub trait ResourceManagerExt {
    async fn allocate_with_guard(
        &self,
        job_id: Uuid,
        plugs: &[String],
    ) -> Result<ResourceGuard, String>;
}

impl ResourceManagerExt for Arc<RwLock<ResourceManager>> {
    async fn allocate_with_guard(
        &self,
        job_id: Uuid,
        plugs: &[String],
    ) -> Result<ResourceGuard, String> {
        {
            let manager = self.write().await;
            manager.allocate_resources(job_id, plugs).await?;
        }

        Ok(ResourceGuard::new(job_id, Arc::clone(self)))
    }
}
