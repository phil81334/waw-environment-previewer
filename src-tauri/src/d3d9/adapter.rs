//! D3D9 reports rendering capabilities. DXGI supplies memory and software
//! adapter information for the same PCI device on hybrid systems.
use serde_json::{json, Value};
use windows::Win32::Graphics::Direct3D9::*;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

#[derive(Clone, Debug)]
pub struct Adapter {
    pub ordinal: u32,
    pub description: String,
    pub vendor_id: u32,
    pub device_id: u32,
    dedicated_memory: usize,
    software: bool,
    shader_model_3: bool,
}
impl Adapter {
    pub fn metadata(&self) -> Value {
        json!({
            "d3d9Ordinal": self.ordinal,
            "description": self.description,
            "vendorId": self.vendor_id,
            "deviceId": self.device_id,
            "dedicatedVideoMemory": self.dedicated_memory,
            "shaderModel3": self.shader_model_3,
            "software": self.software,
            "policy": "Direct3D 9 hardware with shader model 3"
        })
    }
}
pub fn candidates(items: &[Adapter]) -> Result<Vec<&Adapter>, String> {
    let mut eligible: Vec<_> = items
        .iter()
        .filter(|adapter| adapter.shader_model_3 && !adapter.software)
        .collect();
    // Prefer local video memory, while keeping capable shared-memory adapters
    // in the list. Use D3D9's own order to break ties.
    eligible.sort_by_key(|adapter| (std::cmp::Reverse(adapter.dedicated_memory), adapter.ordinal));
    if !eligible.is_empty() {
        return Ok(eligible);
    }
    let available = items
        .iter()
        .map(|adapter| format!("{}: {}", adapter.ordinal, adapter.description))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!("The previewer requires hardware vertex processing and Direct3D 9 vertex and pixel shader model 3. Available adapters: {available}"))
}

fn dxgi_descriptions() -> Vec<DXGI_ADAPTER_DESC1> {
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return Vec::new();
        };
        (0..)
            .map_while(|index| factory.EnumAdapters1(index).ok())
            .filter_map(|adapter| adapter.GetDesc1().ok())
            .collect()
    }
}
pub fn enumerate(factory: &IDirect3D9) -> Result<Vec<Adapter>, String> {
    unsafe {
        let mut adapters = Vec::new();
        let descriptions = dxgi_descriptions();
        for ordinal in 0..factory.GetAdapterCount() {
            let mut info = D3DADAPTER_IDENTIFIER9::default();
            factory
                .GetAdapterIdentifier(ordinal, 0, &mut info)
                .map_err(|e| e.to_string())?;
            let bytes = info
                .Description
                .iter()
                .take_while(|v| **v != 0)
                .map(|v| *v as u8)
                .collect::<Vec<_>>();
            let description = descriptions.iter().find(|adapter| {
                adapter.VendorId == info.VendorId
                    && adapter.DeviceId == info.DeviceId
                    && adapter.SubSysId == info.SubSysId
                    && adapter.Revision == info.Revision
            });
            let mut caps = D3DCAPS9::default();
            let shader_model_3 = factory
                .GetDeviceCaps(ordinal, D3DDEVTYPE_HAL, &mut caps)
                .is_ok()
                && caps.DevCaps & D3DDEVCAPS_HWTRANSFORMANDLIGHT as u32 != 0
                && (caps.VertexShaderVersion & 0xffff) >= 0x0300
                && (caps.PixelShaderVersion & 0xffff) >= 0x0300;
            adapters.push(Adapter {
                ordinal,
                description: String::from_utf8_lossy(&bytes).into_owned(),
                vendor_id: info.VendorId,
                device_id: info.DeviceId,
                dedicated_memory: description.map_or(0, |adapter| adapter.DedicatedVideoMemory),
                software: description.is_some_and(|adapter| {
                    adapter.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
                }),
                shader_model_3,
            });
        }
        Ok(adapters)
    }
}
pub fn inventory() -> Result<Value, String> {
    unsafe {
        let factory = Direct3DCreate9(D3D_SDK_VERSION).ok_or("D3D9 is unavailable")?;
        let adapters = enumerate(&factory)?;
        let eligible = candidates(&adapters);
        Ok(json!({
            "adapters": adapters.iter().map(Adapter::metadata).collect::<Vec<_>>(),
            "selected": eligible.as_ref().ok().and_then(|list| list.first()).map(|adapter| adapter.metadata()),
            "selectionError": eligible.err(),
            "deviceCreated": false
        }))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(ordinal: u32, vendor: u32, dedicated_memory: usize) -> Adapter {
        Adapter {
            ordinal,
            description: format!("GPU{ordinal}"),
            vendor_id: vendor,
            device_id: 0,
            dedicated_memory,
            software: false,
            shader_model_3: true,
        }
    }
    #[test]
    fn prefers_video_memory_across_vendors_and_keeps_fallbacks() {
        let devices = [
            entry(0, 0x8086, 0),
            entry(3, 0x10de, 2048),
            entry(2, 0x1002, 8192),
        ];
        let ranked: Vec<_> = candidates(&devices)
            .unwrap()
            .into_iter()
            .map(|adapter| adapter.ordinal)
            .collect();
        assert_eq!(ranked, [2, 3, 0]);
    }
    #[test]
    fn accepts_integrated_hardware_without_dedicated_memory() {
        let devices = [entry(0, 0x8086, 0)];
        assert_eq!(candidates(&devices).unwrap()[0].ordinal, 0);
    }
    #[test]
    fn excludes_software_and_missing_shader_support() {
        let mut software = entry(0, 0x1414, 16384);
        software.software = true;
        let mut unsupported = entry(1, 0x10de, 4096);
        unsupported.shader_model_3 = false;
        assert!(candidates(&[software, unsupported]).is_err());
    }
}
