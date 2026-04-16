use crate::model::{Device, DeviceId};

#[async_trait::async_trait]
pub trait DeviceStore: Send + Sync + 'static {
    async fn load_all_devices(&self) -> anyhow::Result<Vec<Device>>;
    async fn save_device(&self, device: &Device) -> anyhow::Result<()>;
    async fn delete_device(&self, id: &DeviceId) -> anyhow::Result<()>;
}
