//! Ứng dụng Appx của người dùng hiện tại; không gỡ provisioned package hay gói hệ thống.
//! Chỉ gỡ định danh đã được `list` lưu ở backend, sau khi kiểm tra lại chính sách.

use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub display_name: String,
    pub publisher: String,
    pub version: String,
    pub full_name: String,
    pub family_name: String,
    pub install_location: Option<String>,
    pub built_in: bool,
    pub removable: bool,
}

/// Liệt kê và lưu một snapshot ở backend, kể cả những gói không được phép gỡ.
/// Lỗi truy vấn/JSON không được chuyển thành danh sách rỗng.
#[cfg(windows)]
pub fn list() -> Result<Vec<Package>, String> {
    backend::list()
}

/// Truy vấn đúng PackageFullName cho người dùng hiện tại; lỗi không có nghĩa là đã gỡ.
#[cfg(windows)]
pub fn is_installed(full_name: &str) -> Result<bool, String> {
    backend::is_installed(full_name)
}

/// Gỡ đúng gói đã lưu, kiểm tra lại trước khi gỡ và xác minh bằng truy vấn độc lập.
/// Hủy/quá thời gian luôn trả lỗi, kể cả khi Windows có thể đã thực hiện việc gỡ.
#[cfg(windows)]
pub fn uninstall(full_name: &str, cancel: &AtomicBool) -> Result<(), String> {
    backend::uninstall(full_name, cancel)
}

#[cfg(not(windows))]
pub fn list() -> Result<Vec<Package>, String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(not(windows))]
pub fn is_installed(_full_name: &str) -> Result<bool, String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(not(windows))]
pub fn uninstall(_full_name: &str, _cancel: &AtomicBool) -> Result<(), String> {
    Err("chỉ hỗ trợ Windows".into())
}

#[cfg(any(windows, test))]
mod backend {
    use super::Package;
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(windows)]
    use std::sync::Mutex;
    use std::time::Duration;

    #[cfg(windows)]
    static CACHE: Mutex<Vec<Package>> = Mutex::new(Vec::new());

    // Tên chính xác + publisher id chính thức, không dùng tiền tố "Microsoft.*".
    const INBOX_NAMES: &[&str] = &[
        "Microsoft.WindowsCalculator",
        "Microsoft.WindowsAlarms",
        "Microsoft.WindowsCamera",
        "Microsoft.Windows.Photos",
        "Microsoft.WindowsSoundRecorder",
        "Microsoft.WindowsMaps",
        "Microsoft.GetHelp",
        "Microsoft.Getstarted",
        "Microsoft.MicrosoftSolitaireCollection",
        "microsoft.windowscommunicationsapps",
        "Microsoft.ZuneMusic",
        "Microsoft.ZuneVideo",
    ];

    // Những gói thiết yếu có thể không khai báo NonRemovable trên một số bản Windows.
    // Cùng một danh sách được serialize cho chốt an toàn ngay trong PowerShell.
    const PROTECTED_NAMES: &[&str] = &[
        "Microsoft.WindowsStore",
        "Microsoft.StorePurchaseApp",
        "Microsoft.DesktopAppInstaller",
        "Microsoft.SecHealthUI",
        "Microsoft.Windows.SecHealthUI",
    ];

    #[derive(Debug, Deserialize)]
    struct RawPackage {
        name: String,
        #[serde(default)]
        display_name: Option<String>,
        publisher: String,
        version: String,
        full_name: String,
        family_name: String,
        #[serde(default)]
        install_location: Option<String>,
        #[serde(default)]
        signature_kind: Option<String>,
        #[serde(default)]
        system_location: Option<bool>,
        #[serde(default)]
        non_removable: Option<bool>,
        #[serde(default)]
        is_framework: Option<bool>,
        #[serde(default)]
        is_resource_package: Option<bool>,
        #[serde(default)]
        is_bundle: Option<bool>,
        #[serde(default)]
        status: Option<String>,
    }

    fn named(names: &[&str], name: &str) -> bool {
        names
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(name))
    }

    fn identity_parts(full_name: &str) -> Option<[&str; 5]> {
        let parts: Vec<_> = full_name.split('_').collect();
        let parts: [&str; 5] = parts.try_into().ok()?;
        let version: Vec<_> = parts[1].split('.').collect();
        let word = |s: &str| {
            s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
        };
        if !(3..=50).contains(&parts[0].len())
            || !word(parts[0])
            || version.len() != 4
            || !version.iter().all(|v| {
                !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && v.parse::<u16>().is_ok()
            })
            || parts[2].is_empty()
            || parts[2].len() > 16
            || !parts[2].bytes().all(|c| c.is_ascii_alphanumeric())
            || parts[3].len() > 30
            || !(word(parts[3]) || parts[3] == "~")
            || parts[4].len() != 13
            || !parts[4].bytes().all(|c| c.is_ascii_alphanumeric())
        {
            return None;
        }
        Some(parts)
    }

    fn parse_packages(output: &str) -> Result<Vec<Package>, String> {
        // PowerShell phải xuất mảng cả khi có 0 hoặc 1 gói. Không chấp nhận null/object.
        let rows: Vec<RawPackage> =
            serde_json::from_str(output.trim().trim_start_matches('\u{feff}'))
                .map_err(|e| format!("appx_json: {e}"))?;
        let mut seen = HashSet::new();
        let mut packages = Vec::with_capacity(rows.len());
        for row in rows {
            let parts = identity_parts(&row.full_name).ok_or("appx_invalid_identity")?;
            if row.name != parts[0]
                || row.version != parts[1]
                || row.family_name != format!("{}_{}", parts[0], parts[4])
                || !seen.insert(row.full_name.to_ascii_lowercase())
            {
                return Err("appx_invalid_identity".into());
            }
            let signature = row.signature_kind.as_deref().unwrap_or("");
            let system =
                signature.eq_ignore_ascii_case("System") || row.system_location == Some(true);
            let built_in = system
                || (parts[4].eq_ignore_ascii_case("8wekyb3d8bbwe")
                    && named(INBOX_NAMES, &row.name));
            let install_location = row.install_location.filter(|s| !s.trim().is_empty());
            // Các cờ bị thiếu/null và signature/status không rõ đều khóa gỡ.
            // ResourceId còn chặn tài nguyên/bundle nếu metadata cờ không đáng tin.
            let removable = row.non_removable == Some(false)
                && row.is_framework == Some(false)
                && row.is_resource_package == Some(false)
                && row.is_bundle == Some(false)
                && row.system_location == Some(false)
                && install_location.is_some()
                && named(&["Store", "Developer", "Enterprise"], signature)
                && row
                    .status
                    .as_deref()
                    .is_some_and(|s| s.eq_ignore_ascii_case("Ok"))
                && parts[3].is_empty()
                && named(
                    &["x86", "x64", "arm", "arm64", "neutral", "x86a64"],
                    parts[2],
                )
                && !named(PROTECTED_NAMES, &row.name);
            let display_name = row
                .display_name
                .filter(|s| {
                    !s.trim().is_empty()
                        && !s.trim().to_ascii_lowercase().starts_with("ms-resource:")
                        && !s.trim().starts_with("@{")
                })
                .unwrap_or_else(|| row.name.clone());
            packages.push(Package {
                name: row.name,
                display_name,
                publisher: row.publisher,
                version: row.version,
                full_name: row.full_name,
                family_name: row.family_name,
                install_location,
                built_in,
                removable,
            });
        }
        packages.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then_with(|| a.full_name.cmp(&b.full_name))
        });
        Ok(packages)
    }

    fn query_result(output: &str, full_name: &str) -> Result<Option<Package>, String> {
        let mut packages = parse_packages(output)?;
        if packages.len() > 1
            || packages
                .iter()
                .any(|p| !p.full_name.eq_ignore_ascii_case(full_name))
        {
            return Err("appx_query_mismatch".into());
        }
        Ok(packages.pop())
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Action {
        List,
        Query,
        Remove,
    }

    impl Action {
        fn timeout(self) -> Duration {
            Duration::from_secs(match self {
                Self::List => 60,
                Self::Query => 30,
                Self::Remove => 120,
            })
        }
    }

    #[derive(Serialize)]
    struct Request<'a> {
        full_name: &'a str,
        protected_names: &'static [&'static str],
    }

    // Chỉ nhận JSON đã serialize. Escape Unicode thành JSON \uXXXX còn tránh
    // smart quotes mà PowerShell cũng coi là dấu kết thúc single-quoted literal.
    fn quote_powershell(json: &str) -> String {
        use std::fmt::Write;

        let mut out = String::from("'");
        for c in json.chars() {
            if c == '\'' {
                out.push_str("''");
            } else if c.is_ascii() {
                out.push(c);
            } else {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    write!(&mut out, "\\u{unit:04x}").expect("write to String");
                }
            }
        }
        out.push('\'');
        out
    }

    fn encode_command(script: &str) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            out.push(TABLE[(a >> 2) as usize] as char);
            out.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(c & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    const COMMON: &str = r#"
function Get-CurrentPackages {
    $packages = @(Get-AppxPackage -PackageTypeFilter Main,Framework,Resource,Bundle,Optional -ErrorAction Stop)
    if ($LASTEXITCODE -ne 0) { throw 'appx_query_failed' }
    return $packages
}
function Get-Flag($package, [string]$name) {
    $property = $package.PSObject.Properties[$name]
    if ($null -ne $property -and $property.Value -is [bool]) { return $property.Value }
    return $null
}
function Get-Metadata($package, [bool]$resolveName) {
    $location = [string]$package.InstallLocation
    $systemLocation = $null
    if ([string]::IsNullOrWhiteSpace($location)) {
        $location = $null
    } else {
        try {
            $root = [Environment]::GetFolderPath([Environment+SpecialFolder]::Windows)
            if (-not [string]::IsNullOrWhiteSpace($root)) {
                $root = [IO.Path]::GetFullPath($root).TrimEnd([char]92, [char]47)
                $path = [IO.Path]::GetFullPath($location).TrimEnd([char]92, [char]47)
                $systemLocation = $path.Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
                    $path.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)
            }
        } catch { $systemLocation = $null }
    }
    $displayName = [string]$package.Name
    if ($resolveName) {
        # Lỗi đọc tên chỉ là lỗi trình bày, không che lỗi Get-AppxPackage.
        try {
            $manifest = Get-AppxPackageManifest -Package $package.PackageFullName -ErrorAction Stop
            $label = [string]$manifest.Package.Properties.DisplayName
            if (-not [string]::IsNullOrWhiteSpace($label) -and
                -not $label.StartsWith('ms-resource:', [StringComparison]::OrdinalIgnoreCase) -and
                -not $label.StartsWith('@{', [StringComparison]::Ordinal)) {
                $displayName = $label
            }
        } catch { }
        if ($displayName -eq [string]$package.Name) {
            $prefix = [string]$package.PackageFamilyName + '!'
            $friendly = @($startApps | Where-Object {
                ([string]$_.AppID).StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)
            } | Select-Object -First 1)
            if ($friendly.Count -eq 1 -and -not [string]::IsNullOrWhiteSpace([string]$friendly[0].Name)) {
                $displayName = [string]$friendly[0].Name
            }
        }
    }
    return [pscustomobject]@{
        name = [string]$package.Name
        display_name = $displayName
        publisher = [string]$package.Publisher
        version = [string]$package.Version
        full_name = [string]$package.PackageFullName
        family_name = [string]$package.PackageFamilyName
        install_location = $location
        signature_kind = [string]$package.SignatureKind
        system_location = $systemLocation
        non_removable = Get-Flag $package 'NonRemovable'
        is_framework = Get-Flag $package 'IsFramework'
        is_resource_package = Get-Flag $package 'IsResourcePackage'
        is_bundle = Get-Flag $package 'IsBundle'
        status = [string]$package.Status
    }
}
function Test-Removable($row) {
    $parts = $row.full_name.Split('_')
    if ($parts.Count -ne 5) { return $false }
    return ($row.non_removable -ceq $false -and
        $row.is_framework -ceq $false -and
        $row.is_resource_package -ceq $false -and
        $row.is_bundle -ceq $false -and
        $row.system_location -ceq $false -and
        -not [string]::IsNullOrWhiteSpace($row.install_location) -and
        @('Store', 'Developer', 'Enterprise') -icontains $row.signature_kind -and
        $row.status -ieq 'Ok' -and
        $parts[3] -ceq '' -and
        @('x86', 'x64', 'arm', 'arm64', 'neutral', 'x86a64') -icontains $parts[2] -and
        -not (@($request.protected_names) -icontains $row.name))
}
"#;

    const LIST_SCRIPT: &str = r#"
$packages = @(Get-CurrentPackages)
$startApps = @()
try { $startApps = @(Get-StartApps -ErrorAction Stop) } catch { }
$rows = @($packages | ForEach-Object { Get-Metadata $_ $true })
"#;

    const QUERY_SCRIPT: &str = r#"
$rows = @(Get-CurrentPackages | Where-Object {
    [string]::Equals([string]$_.PackageFullName, $request.full_name, [StringComparison]::OrdinalIgnoreCase)
} | ForEach-Object { Get-Metadata $_ $false })
"#;

    const REMOVE_SCRIPT: &str = r#"
$matches = @(Get-CurrentPackages | Where-Object {
    [string]::Equals([string]$_.PackageFullName, $request.full_name, [StringComparison]::OrdinalIgnoreCase)
})
if ($matches.Count -ne 1) { throw 'appx_package_not_found_or_ambiguous' }
$row = Get-Metadata $matches[0] $false
if (-not (Test-Removable $row)) { throw 'appx_protected_package' }
Remove-AppxPackage -Package $matches[0].PackageFullName -ErrorAction Stop
if ($LASTEXITCODE -ne 0) { throw 'appx_remove_failed' }
$rows = @([pscustomobject]@{ full_name = [string]$matches[0].PackageFullName; completed = $true })
"#;

    fn script(action: Action, full_name: &str) -> Result<String, String> {
        let request = serde_json::to_string(&Request {
            full_name,
            protected_names: PROTECTED_NAMES,
        })
        .map_err(|e| format!("appx_request: {e}"))?;
        let body = match action {
            Action::List => LIST_SCRIPT,
            Action::Query => QUERY_SCRIPT,
            Action::Remove => REMOVE_SCRIPT,
        };
        Ok(format!(
            r#"$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$WarningPreference = 'SilentlyContinue'
$InformationPreference = 'SilentlyContinue'
$LASTEXITCODE = 0
try {{
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $OutputEncoding = [Console]::OutputEncoding
    $request = ConvertFrom-Json -InputObject {} -ErrorAction Stop
{}
{}
    if ($LASTEXITCODE -ne 0) {{ throw 'appx_command_failed' }}
    $json = ConvertTo-Json -InputObject @($rows) -Depth 6 -Compress -ErrorAction Stop
    [Console]::WriteLine($json)
    exit 0
}} catch {{
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}}
"#,
            quote_powershell(&request),
            COMMON,
            body
        ))
    }

    fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::SeqCst) {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    fn run_action(
        action: Action,
        full_name: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<String, String> {
        use crate::winutil::{self, Known};
        use std::time::Instant;

        if let Some(cancel) = cancel {
            check_cancel(cancel)?;
        }
        let source = script(action, full_name)?;
        let encoded = encode_command(&source);
        let root = winutil::known_folder(Known::Windows).ok_or("appx_windows_directory")?;
        let program = root.join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let program = program.to_str().ok_or("appx_powershell_path")?;
        let timeout = action.timeout();
        let started = Instant::now();
        let result = winutil::run_hidden(
            program,
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
                &encoded,
            ],
            timeout,
            cancel,
        );
        // run_hidden kiểm tra exit trước cờ hủy/deadline: chặn cuộc đua thành công ở đây.
        if let Some(cancel) = cancel {
            check_cancel(cancel)?;
        }
        if started.elapsed() >= timeout {
            return Err("timeout".into());
        }
        result
    }

    #[cfg(windows)]
    pub(super) fn list() -> Result<Vec<Package>, String> {
        let mut cache = CACHE.lock().map_err(|_| "appx_cache_poisoned")?;
        cache.clear();
        let packages = parse_packages(&run_action(Action::List, "", None)?)?;
        *cache = packages.clone();
        Ok(packages)
    }

    #[cfg(windows)]
    pub(super) fn is_installed(full_name: &str) -> Result<bool, String> {
        identity_parts(full_name).ok_or("appx_invalid_identity")?;
        Ok(query_result(&run_action(Action::Query, full_name, None)?, full_name)?.is_some())
    }

    #[derive(Deserialize)]
    struct Receipt {
        full_name: String,
        completed: bool,
    }

    fn check_receipt(output: &str, full_name: &str) -> Result<(), String> {
        let receipts: Vec<Receipt> =
            serde_json::from_str(output.trim().trim_start_matches('\u{feff}'))
                .map_err(|e| format!("appx_remove_json: {e}"))?;
        if receipts.len() != 1
            || !receipts[0].completed
            || !receipts[0].full_name.eq_ignore_ascii_case(full_name)
        {
            return Err("appx_remove_unconfirmed".into());
        }
        Ok(())
    }

    // Runner tách riêng để kiểm thử toàn bộ đường gỡ chỉ bằng dữ liệu giả.
    fn uninstall_cached<F>(
        cache: &[Package],
        full_name: &str,
        cancel: &AtomicBool,
        mut run: F,
    ) -> Result<(), String>
    where
        F: FnMut(Action, &str, &AtomicBool) -> Result<String, String>,
    {
        check_cancel(cancel)?;
        let cached = cache
            .iter()
            .find(|p| p.full_name.eq_ignore_ascii_case(full_name))
            .ok_or("appx_not_cached")?;
        if !cached.removable {
            return Err("appx_protected_package".into());
        }
        let target = cached.full_name.as_str(); // Luôn lấy định danh từ backend, không từ caller.
        let output = run(Action::Query, target, cancel)?;
        check_cancel(cancel)?;
        let current = query_result(&output, target)?.ok_or("appx_package_not_found")?;
        if !current.removable {
            return Err("appx_protected_package".into());
        }
        check_cancel(cancel)?;
        let receipt = run(Action::Remove, target, cancel)?;
        check_cancel(cancel)?;
        check_receipt(&receipt, target)?;
        let output = run(Action::Query, target, cancel)?;
        check_cancel(cancel)?;
        if query_result(&output, target)?.is_some() {
            return Err("appx_still_installed".into());
        }
        check_cancel(cancel)
    }

    #[cfg(windows)]
    pub(super) fn uninstall(full_name: &str, cancel: &AtomicBool) -> Result<(), String> {
        check_cancel(cancel)?;
        let cache = CACHE.lock().map_err(|_| "appx_cache_poisoned")?.clone();
        uninstall_cached(&cache, full_name, cancel, |action, target, cancel| {
            run_action(action, target, Some(cancel))
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::{json, Value};

        const FULL: &str = "Microsoft.WindowsCalculator_11.0.0.0_x64__8wekyb3d8bbwe";

        fn row(name: &str, publisher_id: &str) -> Value {
            json!({
                "name": name, "display_name": "Máy tính", "publisher": "CN=Microsoft Corporation",
                "version": "11.0.0.0", "full_name": format!("{name}_11.0.0.0_x64__{publisher_id}"),
                "family_name": format!("{name}_{publisher_id}"),
                "install_location": "C:\\Program Files\\WindowsApps\\Calculator",
                "signature_kind": "Store", "system_location": false, "non_removable": false,
                "is_framework": false, "is_resource_package": false, "is_bundle": false, "status": "Ok"
            })
        }

        fn fixture() -> String {
            json!([row("Microsoft.WindowsCalculator", "8wekyb3d8bbwe")]).to_string()
        }

        fn package(value: Value) -> Package {
            parse_packages(&json!([value]).to_string())
                .unwrap()
                .remove(0)
        }

        fn receipt() -> String {
            json!([{"full_name": FULL, "completed": true}]).to_string()
        }

        #[test]
        fn parses_utf8_arrays_zero_one_many_and_bom() {
            assert!(parse_packages("[]").unwrap().is_empty());
            let packages = parse_packages(&format!("\u{feff}{}", fixture())).unwrap();
            assert_eq!(packages.len(), 1);
            let p = &packages[0];
            assert_eq!(p.display_name, "Máy tính");
            assert_eq!(p.full_name, FULL);
            assert_eq!(p.publisher, "CN=Microsoft Corporation");
            assert_eq!(p.version, "11.0.0.0");
            assert_eq!(p.family_name, "Microsoft.WindowsCalculator_8wekyb3d8bbwe");
            assert!(p.install_location.is_some() && p.built_in && p.removable);
            let many = json!([
                row("Vendor.App", "123456789abcd"),
                row("Microsoft.WindowsCalculator", "8wekyb3d8bbwe")
            ]);
            assert_eq!(parse_packages(&many.to_string()).unwrap().len(), 2);
        }

        #[test]
        fn rejects_non_arrays_invalid_json_wrong_types_and_duplicate_identity() {
            for output in ["", "null", "{}", "[", "access denied", "[null]"] {
                assert!(parse_packages(output).is_err(), "{output}");
            }
            assert!(parse_packages(&row("Vendor.App", "123456789abcd").to_string()).is_err());
            let mut value = row("Vendor.App", "123456789abcd");
            value["is_framework"] = json!("false");
            assert!(parse_packages(&json!([value]).to_string()).is_err());
            assert!(parse_packages(
                &json!([
                    row("Vendor.App", "123456789abcd"),
                    row("Vendor.App", "123456789abcd")
                ])
                .to_string()
            )
            .is_err());
        }

        #[test]
        fn rejects_inconsistent_identity() {
            for field in ["name", "version", "family_name", "full_name"] {
                let mut value = row("Vendor.App", "123456789abcd");
                value[field] = json!("invalid");
                assert!(
                    parse_packages(&json!([value]).to_string()).is_err(),
                    "{field}"
                );
            }
            assert!(identity_parts("x'; Remove-AppxPackage *; #").is_none());
        }

        #[test]
        fn display_and_location_have_safe_fallbacks() {
            for label in [
                Value::Null,
                json!(""),
                json!("ms-resource:AppName"),
                json!("@{indirect}"),
            ] {
                let mut value = row("Vendor.App", "123456789abcd");
                value["display_name"] = label;
                value["install_location"] = json!("  ");
                let p = package(value);
                assert_eq!(p.display_name, "Vendor.App");
                assert!(p.install_location.is_none());
                assert!(!p.removable);
            }
        }

        #[test]
        fn inbox_classification_is_exact_and_requires_official_publisher_id() {
            assert!(!package(row("Microsoft.PowerAutomateDesktop", "8wekyb3d8bbwe")).built_in);
            assert!(!package(row("Microsoft.WindowsCalculator.Extra", "8wekyb3d8bbwe")).built_in);
            assert!(!package(row("Microsoft.WindowsCalculator", "123456789abcd")).built_in);
            assert!(package(row("microsoft.windowscalculator", "8wekyb3d8bbwe")).built_in);
            assert!(!package(row("Vendor.App", "123456789abcd")).built_in);
        }

        #[test]
        fn system_signature_and_location_are_built_in_and_protected() {
            let mut value = row("Vendor.App", "123456789abcd");
            value["signature_kind"] = json!("System");
            let p = package(value);
            assert!(p.built_in && !p.removable);
            let mut value = row("Vendor.App", "123456789abcd");
            value["system_location"] = json!(true);
            let p = package(value);
            assert!(p.built_in && !p.removable);
        }

        #[test]
        fn protection_flags_and_missing_metadata_fail_closed_but_remain_visible() {
            for field in [
                "non_removable",
                "is_framework",
                "is_resource_package",
                "is_bundle",
            ] {
                let mut value = row("Vendor.App", "123456789abcd");
                value[field] = json!(true);
                assert!(!package(value).removable, "{field}");
            }
            for field in [
                "non_removable",
                "is_framework",
                "is_resource_package",
                "is_bundle",
                "system_location",
                "signature_kind",
                "status",
            ] {
                let mut value = row("Vendor.App", "123456789abcd");
                value.as_object_mut().unwrap().remove(field);
                assert!(!package(value.clone()).removable, "missing {field}");
                value[field] = Value::Null;
                assert!(!package(value).removable, "null {field}");
            }
            for name in PROTECTED_NAMES {
                assert!(!package(row(name, "8wekyb3d8bbwe")).removable, "{name}");
            }
        }

        #[test]
        fn resource_ids_bundles_unknown_signature_and_bad_status_are_protected() {
            for resource in ["en-US", "~"] {
                let mut value = row("Vendor.App", "123456789abcd");
                value["full_name"] =
                    json!(format!("Vendor.App_11.0.0.0_x64_{resource}_123456789abcd"));
                assert!(!package(value).removable);
            }
            for (field, val) in [("signature_kind", "Unknown"), ("status", "Modified")] {
                let mut value = row("Vendor.App", "123456789abcd");
                value[field] = json!(val);
                assert!(!package(value).removable);
            }
        }

        #[test]
        fn quotes_serialized_untrusted_metadata_as_literal_and_encodes_utf16le() {
            let hostile = "x'; ‘’; ‚‛; Remove-AppxPackage *; #\r\n$(`whoami`)\\\"Máy𠀀";
            let request = serde_json::to_string(&Request {
                full_name: hostile,
                protected_names: PROTECTED_NAMES,
            })
            .unwrap();
            let literal = quote_powershell(&request);
            let decoded: Value =
                serde_json::from_str(&literal[1..literal.len() - 1].replace("''", "'")).unwrap();
            assert_eq!(decoded["full_name"], hostile);
            assert!(literal.is_ascii());
            assert!(literal.contains("x'';"));
            let source = script(Action::Query, hostile).unwrap();
            assert!(source.contains(&format!(
                "ConvertFrom-Json -InputObject {literal} -ErrorAction Stop"
            )));
            assert_eq!(encode_command(""), "");
            assert_eq!(encode_command("A"), "QQA=");
            assert_eq!(encode_command("ABC"), "QQBCAEMA");
            assert_eq!(encode_command("é"), "6QA=");
            assert_eq!(encode_command("𠀀"), "QNgA3A==");
            assert!(encode_command(&source)
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c)));
        }

        #[test]
        fn scripts_use_only_current_user_exact_target_without_bypasses() {
            for action in [Action::List, Action::Query, Action::Remove] {
                let source = script(action, FULL).unwrap();
                for forbidden in [
                    "-AllUsers",
                    "-User ",
                    "Provisioned",
                    "ExecutionPolicy",
                    "Invoke-Expression",
                    "Start-Process",
                    "-Force",
                ] {
                    assert!(!source.contains(forbidden), "{forbidden}");
                }
                assert!(source.contains("-ErrorAction Stop"));
                assert!(source.contains("$LASTEXITCODE -ne 0"));
                assert!(source.contains("ConvertTo-Json -InputObject @($rows)"));
                assert!(source.contains("System.Text.UTF8Encoding($false)"));
                assert!(source.contains("exit 1"));
                assert!(encode_command(&source).len() < 30_000);
                assert!(action.timeout() <= Duration::from_secs(120));
            }
            assert!(!script(Action::List, "")
                .unwrap()
                .contains("Remove-AppxPackage"));
            assert!(!script(Action::Query, FULL)
                .unwrap()
                .contains("Remove-AppxPackage"));
            let source = script(Action::Remove, FULL).unwrap();
            assert!(source.contains("if (-not (Test-Removable $row))"));
            assert!(source.contains(
                "Remove-AppxPackage -Package $matches[0].PackageFullName -ErrorAction Stop"
            ));
        }

        #[cfg(windows)]
        #[test]
        #[ignore = "Windows PowerShell: chỉ parse script và kiểm tra metadata giả"]
        fn powershell_syntax_and_policy_with_synthetic_metadata_only() {
            use crate::winutil::{self, Known};

            let program = winutil::known_folder(Known::Windows)
                .unwrap()
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let validate = |source: &str| {
                winutil::run_hidden(
                    program.to_str().unwrap(),
                    &[
                        "-NoLogo",
                        "-NoProfile",
                        "-NonInteractive",
                        "-EncodedCommand",
                        &encode_command(source),
                    ],
                    Duration::from_secs(15),
                    None,
                )
                .unwrap()
            };
            // Parse từng script để không vượt giới hạn dòng lệnh 32K của Windows.
            for action in [Action::List, Action::Query, Action::Remove] {
                let source = script(action, "x'; ‘’; ‚‛; #\r\n$(`whoami`)\\\"Máy𠀀").unwrap();
                let source = quote_powershell(&serde_json::to_string(&source).unwrap());
                let parser = format!(
                    r#"$ErrorActionPreference = 'Stop'
try {{
    $source = ConvertFrom-Json -InputObject {source} -ErrorAction Stop
    $tokens = $null
    $errors = $null
    $null = [System.Management.Automation.Language.Parser]::ParseInput($source, [ref]$tokens, [ref]$errors)
    if ($errors.Count -ne 0) {{ throw ($errors | Out-String) }}
    [Console]::WriteLine('[]')
    exit 0
}} catch {{
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}}
"#
                );
                assert_eq!(validate(&parser).trim(), "[]");
            }
            let request = quote_powershell(
                &serde_json::to_string(&Request {
                    full_name: FULL,
                    protected_names: PROTECTED_NAMES,
                })
                .unwrap(),
            );
            // COMMON chỉ định nghĩa hàm; Get-Metadata nhận metadata giả và
            // resolveName=false, không gọi Get-CurrentPackages hay cmdlet Appx.
            let validator = format!(
                r#"$ErrorActionPreference = 'Stop'
try {{
    [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
    $request = ConvertFrom-Json -InputObject {request} -ErrorAction Stop
{COMMON}
    $fake = [pscustomobject]@{{
        Name = 'Microsoft.WindowsCalculator'
        PackageFullName = '{FULL}'
        PackageFamilyName = 'Microsoft.WindowsCalculator_8wekyb3d8bbwe'
        Publisher = 'CN=Microsoft Corporation'
        Version = '11.0.0.0'
        InstallLocation = 'C:\TPStoreAppsSyntheticTest'
        SignatureKind = 'Store'
        NonRemovable = $false
        IsFramework = $false
        IsResourcePackage = $false
        IsBundle = $false
        Status = 'Ok'
    }}
    if (-not (Test-Removable (Get-Metadata $fake $false))) {{ throw 'synthetic_main_blocked' }}
    foreach ($flag in @('NonRemovable', 'IsFramework', 'IsResourcePackage', 'IsBundle')) {{
        $fake.$flag = $true
        if (Test-Removable (Get-Metadata $fake $false)) {{ throw ('synthetic_protection_failed: ' + $flag) }}
        $fake.$flag = $null
        if (Test-Removable (Get-Metadata $fake $false)) {{ throw ('synthetic_null_flag_allowed: ' + $flag) }}
        $fake.$flag = $false
    }}
    $fake.PSObject.Properties.Remove('IsFramework')
    if (Test-Removable (Get-Metadata $fake $false)) {{ throw 'synthetic_missing_flag_allowed' }}
    $fake | Add-Member -NotePropertyName IsFramework -NotePropertyValue $false
    $fake.SignatureKind = 'System'
    if (Test-Removable (Get-Metadata $fake $false)) {{ throw 'synthetic_system_signature_allowed' }}
    $fake.SignatureKind = 'Store'
    $fake.InstallLocation = [Environment]::GetFolderPath([Environment+SpecialFolder]::Windows) + '\SystemApps\Synthetic'
    $row = Get-Metadata $fake $false
    if ($row.system_location -cne $true -or (Test-Removable $row)) {{ throw 'synthetic_windows_location_allowed' }}
    $fake.InstallLocation = 'C:\WindowsAppsNotSystem\Synthetic'
    if (-not (Test-Removable (Get-Metadata $fake $false))) {{ throw 'synthetic_location_prefix_mismatch' }}
    foreach ($name in $request.protected_names) {{
        $fake.Name = $name
        if (Test-Removable (Get-Metadata $fake $false)) {{ throw ('synthetic_essential_allowed: ' + $name) }}
    }}
    [Console]::WriteLine('[]')
    exit 0
}} catch {{
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}}
"#
            );
            assert_eq!(validate(&validator).trim(), "[]");
        }

        #[test]
        fn queries_only_prove_absence_from_valid_empty_array() {
            assert!(query_result("[]", FULL).unwrap().is_none());
            assert!(query_result(&fixture(), FULL).unwrap().is_some());
            assert!(query_result("null", FULL).is_err());
            assert!(query_result("[] garbage", FULL).is_err());
            assert!(query_result(
                &json!([row("Vendor.App", "123456789abcd")]).to_string(),
                FULL
            )
            .is_err());
        }

        #[test]
        fn rejects_uncached_protected_and_pre_cancelled_without_running_any_process() {
            let cache = parse_packages(&fixture()).unwrap();
            let never = |_, _: &str, _: &AtomicBool| -> Result<String, String> {
                panic!("runner must not be called")
            };
            assert!(uninstall_cached(&[], FULL, &AtomicBool::new(false), never).is_err());
            assert!(uninstall_cached(&cache, "*", &AtomicBool::new(false), never).is_err());
            assert_eq!(
                uninstall_cached(&cache, FULL, &AtomicBool::new(true), never).unwrap_err(),
                "cancelled"
            );
            let mut protected = cache;
            protected[0].removable = false;
            assert_eq!(
                uninstall_cached(&protected, FULL, &AtomicBool::new(false), never).unwrap_err(),
                "appx_protected_package"
            );
        }

        #[test]
        fn rechecks_policy_before_any_removal() {
            let cache = parse_packages(&fixture()).unwrap();
            let mut value = row("Microsoft.WindowsCalculator", "8wekyb3d8bbwe");
            value["non_removable"] = json!(true);
            let result = uninstall_cached(&cache, FULL, &AtomicBool::new(false), |action, _, _| {
                assert_eq!(action, Action::Query);
                Ok(json!([value]).to_string())
            });
            assert_eq!(result.unwrap_err(), "appx_protected_package");
        }

        #[test]
        fn successful_mock_removal_requires_receipt_and_independent_absence_query() {
            let cache = parse_packages(&fixture()).unwrap();
            let mut step = 0;
            uninstall_cached(
                &cache,
                &FULL.to_ascii_lowercase(),
                &AtomicBool::new(false),
                |action, target, _| {
                    assert_eq!(target, FULL);
                    let output = match step {
                        0 => {
                            assert_eq!(action, Action::Query);
                            fixture()
                        }
                        1 => {
                            assert_eq!(action, Action::Remove);
                            receipt()
                        }
                        2 => {
                            assert_eq!(action, Action::Query);
                            "[]".into()
                        }
                        _ => panic!("unexpected call"),
                    };
                    step += 1;
                    Ok(output)
                },
            )
            .unwrap();
            assert_eq!(step, 3);
        }

        #[test]
        fn query_and_removal_errors_timeouts_and_bad_receipts_never_succeed() {
            let cache = parse_packages(&fixture()).unwrap();
            for failing_step in 0..3 {
                for error in ["timeout", "cancelled", "access denied"] {
                    let mut step = 0;
                    let result =
                        uninstall_cached(&cache, FULL, &AtomicBool::new(false), |_, _, _| {
                            let current = step;
                            step += 1;
                            if current == failing_step {
                                Err(error.into())
                            } else if current == 0 {
                                Ok(fixture())
                            } else {
                                Ok(receipt())
                            }
                        });
                    assert_eq!(result.unwrap_err(), error);
                    assert_eq!(step, failing_step + 1);
                }
            }
            for output in [
                "",
                "[]",
                "null",
                "[{\"full_name\":\"other\",\"completed\":true}]",
                "[{\"full_name\":\"other\",\"completed\":false}]",
            ] {
                assert!(check_receipt(output, FULL).is_err());
            }
            let mut step = 0;
            let result = uninstall_cached(&cache, FULL, &AtomicBool::new(false), |_, _, _| {
                step += 1;
                Ok(match step {
                    1 => fixture(),
                    2 => receipt(),
                    _ => "null".into(),
                })
            });
            assert!(result.is_err());
        }

        #[test]
        fn still_installed_or_cancel_racing_each_successful_step_is_not_success() {
            let cache = parse_packages(&fixture()).unwrap();
            let mut step = 0;
            let result = uninstall_cached(&cache, FULL, &AtomicBool::new(false), |_, _, _| {
                step += 1;
                Ok(if step == 2 { receipt() } else { fixture() })
            });
            assert_eq!(result.unwrap_err(), "appx_still_installed");
            for cancel_step in 0..3 {
                let cancel = AtomicBool::new(false);
                let mut step = 0;
                let result = uninstall_cached(&cache, FULL, &cancel, |_, _, flag| {
                    let current = step;
                    step += 1;
                    if current == cancel_step {
                        flag.store(true, Ordering::SeqCst);
                    }
                    Ok(match current {
                        0 => fixture(),
                        1 => receipt(),
                        _ => "[]".into(),
                    })
                });
                assert_eq!(result.unwrap_err(), "cancelled");
                assert_eq!(step, cancel_step + 1);
            }
        }
    }
}

#[cfg(all(test, not(windows)))]
mod stub_tests {
    use super::*;

    #[test]
    fn non_windows_returns_unsupported_not_absence_or_success() {
        assert!(list().is_err());
        assert!(is_installed("anything").is_err());
        assert!(uninstall("anything", &AtomicBool::new(false)).is_err());
    }
}
