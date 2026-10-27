//! Vulkan device and driver identity. `pipelineCacheUUID` changes whenever the driver's
//! compiler changes, so it is the key that tells us a game's cache went stale.

use std::ffi::CStr;
use std::fmt;

use anyhow::{Context, Result};
use ash::vk;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Amd,
    Nvidia,
    Intel,
    Other(u32),
}

impl Vendor {
    fn from_id(id: u32) -> Self {
        match id {
            0x1002 => Vendor::Amd,
            0x10de => Vendor::Nvidia,
            0x8086 => Vendor::Intel,
            other => Vendor::Other(other),
        }
    }
}

impl fmt::Display for Vendor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Vendor::Amd => f.write_str("AMD"),
            Vendor::Nvidia => f.write_str("NVIDIA"),
            Vendor::Intel => f.write_str("Intel"),
            Vendor::Other(id) => write!(f, "vendor {id:#06x}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Gpu {
    pub name: String,
    pub vendor: Vendor,
    pub device_id: u32,
    pub device_type: vk::PhysicalDeviceType,
    /// e.g. `radv`, `NVIDIA`, `Intel open-source Mesa driver`.
    pub driver_name: String,
    /// e.g. `Mesa 26.2.5-arch1.1`.
    pub driver_info: String,
    pub pipeline_cache_uuid: [u8; vk::UUID_SIZE],
}

impl Gpu {
    pub fn uuid_hex(&self) -> String {
        self.pipeline_cache_uuid.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn is_software(&self) -> bool {
        self.device_type == vk::PhysicalDeviceType::CPU
    }
}

fn c_str(bytes: Result<&CStr, std::ffi::FromBytesUntilNulError>) -> String {
    bytes.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// All Vulkan devices visible to this process, in loader order.
pub fn enumerate() -> Result<Vec<Gpu>> {
    // SAFETY: loads the system Vulkan loader; the entry outlives every call below.
    let entry = unsafe { ash::Entry::load() }.context("loading the Vulkan loader (libvulkan.so.1)")?;
    let app = vk::ApplicationInfo::default()
        .application_name(c"shaderwarm")
        .api_version(vk::API_VERSION_1_2);
    let create_info = vk::InstanceCreateInfo::default().application_info(&app);
    // SAFETY: create_info only borrows locals that live until the call returns.
    let instance = unsafe { entry.create_instance(&create_info, None) }.context("creating a Vulkan instance")?;

    let result = (|| {
        // SAFETY: the instance is valid until destroy_instance below.
        let devices = unsafe { instance.enumerate_physical_devices() }?;
        let gpus = devices
            .into_iter()
            .map(|pd| {
                let mut driver = vk::PhysicalDeviceDriverProperties::default();
                let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
                // SAFETY: pd comes from this instance; props2 chains only `driver`.
                unsafe { instance.get_physical_device_properties2(pd, &mut props2) };
                let props = props2.properties;
                Gpu {
                    name: c_str(props.device_name_as_c_str()),
                    vendor: Vendor::from_id(props.vendor_id),
                    device_id: props.device_id,
                    device_type: props.device_type,
                    driver_name: c_str(driver.driver_name_as_c_str()),
                    driver_info: c_str(driver.driver_info_as_c_str()),
                    pipeline_cache_uuid: props.pipeline_cache_uuid,
                }
            })
            .collect();
        Ok::<_, vk::Result>(gpus)
    })();

    // SAFETY: no child objects were created from the instance.
    unsafe { instance.destroy_instance(None) };
    result.context("querying Vulkan devices")
}

/// The device games will most likely run on: the first discrete GPU, then any hardware GPU.
pub fn primary(gpus: &[Gpu]) -> Option<&Gpu> {
    gpus.iter()
        .find(|g| g.device_type == vk::PhysicalDeviceType::DISCRETE_GPU)
        .or_else(|| gpus.iter().find(|g| !g.is_software()))
}
