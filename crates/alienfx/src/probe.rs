//! Read-only HID probe for the AlienFX lighting devices (Windows).
//!
//! Enumerates present HID interfaces with SetupDi, and for interfaces whose path names VID 187C or 0D62 only:
//! opens them with desired access 0 (query only: no read, no write), calls `HidD_GetAttributes`,
//! `HidD_GetPreparsedData` + `HidP_GetCaps`, and closes the handle. These calls are answered by the HID class
//! driver from data it already holds; no report of any kind (output, feature, input, GET or SET) is sent, and
//! no `WriteFile`/`ReadFile` is issued. This is the only module of the crate that uses `unsafe`.

use crate::detect::{classify, ApiVersion, VID_ALIENWARE, VID_DARFON};
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SPINT_ACTIVE,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetPreparsedData, HidP_GetCaps, GUID_DEVINTERFACE_HID,
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// The vendors this probe opens. Every other interface is skipped by path before any handle is opened.
pub const PROBED_VIDS: [u16; 2] = [VID_ALIENWARE, VID_DARFON];

/// What the probe learned about one HID interface (one top-level collection).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HidInterface {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub version: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub input_len: u16,
    pub output_len: u16,
    pub feature_len: u16,
    /// alien-tools' classification ([`classify`]); `None` for collections it would skip.
    pub api: Option<ApiVersion>,
    /// The report length alien-tools would use for that API.
    pub report_len: Option<u16>,
    /// Set when the interface was found but could not be queried (open or caps failed).
    pub error: Option<String>,
}

/// Interfaces that were listed but skipped or failed are reported here too, so the output is complete.
#[derive(Clone, Debug, Default)]
pub struct ProbeResult {
    pub interfaces: Vec<HidInterface>,
    /// Count of present HID interfaces seen (all vendors).
    pub hid_interfaces_seen: usize,
}

struct DevInfo(HDEVINFO);
impl Drop for DevInfo {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupDiGetClassDevsW and is destroyed once.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from CreateFileW and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `vid_xxxx` from a HID interface path, e.g. `\\?\hid#vid_187c&pid_0550#...`.
fn path_vid(path: &str) -> Option<u16> {
    let lower = path.to_ascii_lowercase();
    let i = lower.find("vid_")?;
    u16::from_str_radix(lower.get(i + 4..i + 8)?, 16).ok()
}

fn interface_path(set: &DevInfo, data: &SP_DEVICE_INTERFACE_DATA) -> Option<Vec<u16>> {
    let mut required = 0u32;
    // SAFETY: size query with no buffer; it fails with ERROR_INSUFFICIENT_BUFFER and fills `required`.
    let _ = unsafe { SetupDiGetDeviceInterfaceDetailW(set.0, data, None, 0, Some(&mut required), None) };
    let cb = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
    if (required as usize) < cb {
        return None;
    }
    // u32-aligned buffer large enough for the detail struct and its trailing path.
    let mut buf = vec![0u32; (required as usize).div_ceil(4)];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // SAFETY: `buf` is at least `required` bytes and 4-byte aligned; cbSize is the fixed struct size.
    unsafe {
        (*detail).cbSize = cb as u32;
        SetupDiGetDeviceInterfaceDetailW(set.0, data, Some(detail), required, None, None).ok()?;
        let path_ptr = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
        let max = (required as usize - 4) / 2;
        let path = std::slice::from_raw_parts(path_ptr, max);
        let len = path.iter().position(|&c| c == 0).unwrap_or(max);
        let mut v = path[..len].to_vec();
        v.push(0);
        Some(v)
    }
}

/// Query-only open (desired access 0) plus attributes and caps. No report is sent or read.
fn query(path_w: &[u16]) -> Result<(HIDD_ATTRIBUTES, HIDP_CAPS), String> {
    // SAFETY: `path_w` is NUL-terminated. Access 0 opens the interface for attribute queries only.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_w.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .map_err(|e| format!("open (access 0) failed: {e}"))?;
    let handle = Handle(handle);

    let mut attrs = HIDD_ATTRIBUTES { Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32, ..Default::default() };
    // SAFETY: valid handle and a correctly sized out struct.
    if !unsafe { HidD_GetAttributes(handle.0, &mut attrs) } {
        return Err("HidD_GetAttributes failed".into());
    }
    let mut prep = PHIDP_PREPARSED_DATA::default();
    // SAFETY: valid handle; on success `prep` is freed below.
    if !unsafe { HidD_GetPreparsedData(handle.0, &mut prep) } {
        return Err("HidD_GetPreparsedData failed".into());
    }
    let mut caps = HIDP_CAPS::default();
    // SAFETY: `prep` is valid preparsed data; it is freed exactly once.
    let status = unsafe { HidP_GetCaps(prep, &mut caps) };
    unsafe {
        let _ = HidD_FreePreparsedData(prep);
    }
    if status != HIDP_STATUS_SUCCESS {
        return Err(format!("HidP_GetCaps failed: 0x{:08x}", status.0 as u32));
    }
    Ok((attrs, caps))
}

/// Enumerates present HID interfaces and queries those of VID 187C and 0D62. Needs no admin rights.
pub fn probe() -> Result<ProbeResult, String> {
    // SAFETY: standard SetupDi enumeration of present HID device interfaces.
    let set = unsafe {
        SetupDiGetClassDevsW(Some(&GUID_DEVINTERFACE_HID), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
    }
    .map_err(|e| format!("SetupDiGetClassDevsW failed: {e}"))?;
    let set = DevInfo(set);

    let mut out = ProbeResult::default();
    for index in 0u32.. {
        let mut data =
            SP_DEVICE_INTERFACE_DATA { cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
        // SAFETY: `data.cbSize` is set; the call fails with ERROR_NO_MORE_ITEMS past the last interface.
        if unsafe { SetupDiEnumDeviceInterfaces(set.0, None, &GUID_DEVINTERFACE_HID, index, &mut data) }.is_err() {
            break;
        }
        if data.Flags & SPINT_ACTIVE == 0 {
            continue;
        }
        out.hid_interfaces_seen += 1;
        let Some(path_w) = interface_path(&set, &data) else { continue };
        let path = String::from_utf16_lossy(&path_w[..path_w.len() - 1]);
        let Some(vid) = path_vid(&path).filter(|v| PROBED_VIDS.contains(v)) else { continue };

        let mut iface = HidInterface {
            path,
            vid,
            pid: 0,
            version: 0,
            usage_page: 0,
            usage: 0,
            input_len: 0,
            output_len: 0,
            feature_len: 0,
            api: None,
            report_len: None,
            error: None,
        };
        match query(&path_w) {
            Ok((a, c)) => {
                iface.vid = a.VendorID;
                iface.pid = a.ProductID;
                iface.version = a.VersionNumber;
                iface.usage_page = c.UsagePage;
                iface.usage = c.Usage;
                iface.input_len = c.InputReportByteLength;
                iface.output_len = c.OutputReportByteLength;
                iface.feature_len = c.FeatureReportByteLength;
                if let Some(d) = classify(iface.vid, iface.pid, iface.usage, iface.output_len, iface.feature_len) {
                    iface.api = Some(d.version);
                    iface.report_len = Some(d.report_len);
                }
            }
            Err(e) => iface.error = Some(e),
        }
        out.interfaces.push(iface);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::path_vid;

    #[test]
    fn vid_from_path() {
        assert_eq!(path_vid(r"\\?\hid#vid_187c&pid_0550#7&1&0&0000#{4d1e55b2}"), Some(0x187c));
        assert_eq!(path_vid(r"\\?\HID#VID_0D62&PID_CABC&MI_02&Col02#8&2#{4d1e55b2}"), Some(0x0d62));
        assert_eq!(path_vid(r"\\?\hid#acpi0c50#1"), None);
    }
}
