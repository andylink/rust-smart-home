use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{AttributeValue, Device, DeviceId, Room, RoomId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceHistoryEntry {
    pub observed_at: DateTime<Utc>,
    pub device: Device,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttributeHistoryEntry {
    pub observed_at: DateTime<Utc>,
    pub device_id: DeviceId,
    pub attribute: String,
    pub value: AttributeValue,
}

#[async_trait::async_trait]
pub trait DeviceStore: Send + Sync + 'static {
    async fn load_all_devices(&self) -> anyhow::Result<Vec<Device>>;
    async fn load_all_rooms(&self) -> anyhow::Result<Vec<Room>>;
    async fn save_device(&self, device: &Device) -> anyhow::Result<()>;
    async fn save_room(&self, room: &Room) -> anyhow::Result<()>;
    async fn delete_device(&self, id: &DeviceId) -> anyhow::Result<()>;
    async fn delete_room(&self, id: &RoomId) -> anyhow::Result<()>;
    async fn load_device_history(
        &self,
        id: &DeviceId,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<DeviceHistoryEntry>>;
    async fn load_attribute_history(
        &self,
        id: &DeviceId,
        attribute: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
        limit: usize,
    ) -> anyhow::Result<Vec<AttributeHistoryEntry>>;
}
