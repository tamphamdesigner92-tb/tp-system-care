//! Đọc/ghi registry ở mức thô, dùng chung cho các module và Rescue Center.
//! Luôn làm việc trên view 64-bit (tiến trình là 64-bit).

use serde::{Deserialize, Serialize};

/// Một giá trị registry kèm kiểu, đủ để ghi lại y nguyên khi hoàn tác.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawValue {
    /// Mã kiểu của Windows (1 = REG_SZ, 3 = REG_BINARY, 4 = REG_DWORD, ...).
    pub vtype: u32,
    pub bytes: Vec<u8>,
}

pub const REG_SZ: u32 = 1;
pub const REG_BINARY: u32 = 3;
pub const REG_DWORD: u32 = 4;

impl RawValue {
    pub fn dword(value: u32) -> Self {
        Self {
            vtype: REG_DWORD,
            bytes: value.to_le_bytes().to_vec(),
        }
    }

    pub fn sz(value: &str) -> Self {
        let bytes = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        Self { vtype: REG_SZ, bytes }
    }

    pub fn as_dword(&self) -> Option<u32> {
        (self.vtype == REG_DWORD && self.bytes.len() >= 4)
            .then(|| u32::from_le_bytes([self.bytes[0], self.bytes[1], self.bytes[2], self.bytes[3]]))
    }

    /// Chuỗi của giá trị REG_SZ / REG_EXPAND_SZ.
    pub fn as_string(&self) -> Option<String> {
        if self.vtype != REG_SZ && self.vtype != 2 {
            return None;
        }
        let units: Vec<u16> = self
            .bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|unit| *unit != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }
}

pub fn hive_name(machine: bool) -> &'static str {
    if machine {
        "HKLM"
    } else {
        "HKCU"
    }
}

#[cfg(windows)]
mod imp {
    use super::RawValue;
    use std::borrow::Cow;
    use std::io::ErrorKind;
    use winreg::enums::*;
    use winreg::{RegKey, RegValue};

    fn hive(machine: bool) -> RegKey {
        RegKey::predef(if machine { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER })
    }

    fn describe(e: std::io::Error) -> String {
        if e.kind() == ErrorKind::PermissionDenied {
            "needs_admin".into()
        } else {
            e.to_string()
        }
    }

    fn to_type(code: u32) -> RegType {
        match code {
            0 => REG_NONE,
            1 => REG_SZ,
            2 => REG_EXPAND_SZ,
            4 => REG_DWORD,
            7 => REG_MULTI_SZ,
            11 => REG_QWORD,
            _ => REG_BINARY,
        }
    }

    fn from_winreg(value: RegValue) -> RawValue {
        RawValue {
            vtype: value.vtype as u32,
            bytes: value.bytes.into_owned(),
        }
    }

    pub fn read(machine: bool, subkey: &str, name: &str) -> Option<RawValue> {
        let key = hive(machine).open_subkey(subkey).ok()?;
        key.get_raw_value(name).ok().map(from_winreg)
    }

    pub fn write(machine: bool, subkey: &str, name: &str, value: &RawValue) -> Result<(), String> {
        let (key, _) = hive(machine).create_subkey(subkey).map_err(describe)?;
        key.set_raw_value(
            name,
            &RegValue {
                bytes: Cow::Borrowed(&value.bytes),
                vtype: to_type(value.vtype),
            },
        )
        .map_err(describe)
    }

    /// Xóa một giá trị; không tồn tại thì coi như đã xong.
    pub fn delete_value(machine: bool, subkey: &str, name: &str) -> Result<(), String> {
        let key = match hive(machine).open_subkey_with_flags(subkey, KEY_SET_VALUE) {
            Ok(key) => key,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(describe(e)),
        };
        match key.delete_value(name) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(describe(e)),
            _ => Ok(()),
        }
    }

    /// Mọi giá trị của một khóa (tên rỗng là giá trị mặc định).
    pub fn values(machine: bool, subkey: &str) -> Vec<(String, RawValue)> {
        let Ok(key) = hive(machine).open_subkey(subkey) else {
            return Vec::new();
        };
        key.enum_values()
            .filter_map(Result::ok)
            .map(|(name, value)| (name, from_winreg(value)))
            .collect()
    }

    pub fn subkeys(machine: bool, subkey: &str) -> Vec<String> {
        let Ok(key) = hive(machine).open_subkey(subkey) else {
            return Vec::new();
        };
        key.enum_keys().filter_map(Result::ok).collect()
    }

    pub fn has_subkeys(machine: bool, subkey: &str) -> bool {
        !subkeys(machine, subkey).is_empty()
    }

    pub fn delete_key(machine: bool, subkey: &str) -> Result<(), String> {
        match hive(machine).delete_subkey_all(subkey) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(describe(e)),
            _ => Ok(()),
        }
    }

    /// Thời điểm khóa được ghi lần cuối, tính bằng giây Unix (UTC).
    pub fn last_write(machine: bool, subkey: &str) -> Option<u64> {
        let info = hive(machine).open_subkey(subkey).ok()?.query_info().ok()?;
        let st = info.get_last_write_time_system();
        super::unix_from_civil(st.wYear as i64, st.wMonth as i64, st.wDay as i64, st.wHour as i64, st.wMinute as i64, st.wSecond as i64)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::RawValue;
    const UNSUPPORTED: &str = "chỉ hỗ trợ Windows";

    pub fn read(_: bool, _: &str, _: &str) -> Option<RawValue> {
        None
    }
    pub fn write(_: bool, _: &str, _: &str, _: &RawValue) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn delete_value(_: bool, _: &str, _: &str) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn values(_: bool, _: &str) -> Vec<(String, RawValue)> {
        Vec::new()
    }
    pub fn subkeys(_: bool, _: &str) -> Vec<String> {
        Vec::new()
    }
    pub fn has_subkeys(_: bool, _: &str) -> bool {
        false
    }
    pub fn delete_key(_: bool, _: &str) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub fn last_write(_: bool, _: &str) -> Option<u64> {
        None
    }
}

pub use imp::*;

/// Đổi ngày giờ UTC sang giây Unix; `None` nếu trước 1970 hoặc không hợp lệ.
pub(crate) fn unix_from_civil(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Thuật toán days_from_civil của Howard Hinnant.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second).ok()
}

/// Ghi lại giá trị cũ, hoặc xóa nếu trước đó không có.
pub fn restore(machine: bool, subkey: &str, name: &str, previous: Option<&RawValue>) -> Result<(), String> {
    match previous {
        Some(value) => write(machine, subkey, name, value),
        None => delete_value(machine, subkey, name),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_convert_to_unix_seconds() {
        assert_eq!(unix_from_civil(1970, 1, 1, 0, 0, 0), Some(0));
        assert_eq!(unix_from_civil(2000, 3, 1, 0, 0, 0), Some(951_868_800));
        assert_eq!(unix_from_civil(2026, 11, 19, 12, 30, 5), Some(1_795_091_405));
        assert_eq!(unix_from_civil(1969, 12, 31, 0, 0, 0), None);
        assert!(last_write(false, "Software").is_some());
    }

    #[test]
    fn raw_value_round_trips() {
        assert_eq!(RawValue::dword(0xFFFF_FFFF).as_dword(), Some(0xFFFF_FFFF));
        assert_eq!(RawValue::sz("C:\\A b").as_string().as_deref(), Some("C:\\A b"));
        assert_eq!(RawValue::sz("x").as_dword(), None);
    }

    #[test]
    fn write_read_restore_and_delete_key() {
        let base = format!("Software\\TP System Care Test\\reg-{}", std::process::id());
        let key = format!("{base}\\Sub");

        write(false, &key, "Number", &RawValue::dword(7)).unwrap();
        write(false, &key, "", &RawValue::sz("default")).unwrap();
        assert_eq!(read(false, &key, "Number").unwrap().as_dword(), Some(7));
        assert_eq!(values(false, &key).len(), 2);
        assert_eq!(subkeys(false, &base), vec!["Sub".to_string()]);

        let previous = read(false, &key, "Number");
        write(false, &key, "Number", &RawValue::dword(9)).unwrap();
        restore(false, &key, "Number", previous.as_ref()).unwrap();
        assert_eq!(read(false, &key, "Number").unwrap().as_dword(), Some(7));

        restore(false, &key, "Number", None).unwrap();
        assert!(read(false, &key, "Number").is_none());

        delete_key(false, &base).unwrap();
        assert!(values(false, &key).is_empty());
        delete_key(false, &base).unwrap();
    }
}
