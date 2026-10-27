//! # DevLink MCP Credentials Management
//!
//! MCP(Model Context Protocol) 서버와 로컬 브로커 간의 비공개 연결 자격 파일(`broker.json`)을
//! 생성, 검증, 원자적 발행 및 회전(rotate)하는 모듈입니다.
//!
//! - `packages/mcp-server/src/broker/settings.ts`의 `settingsSchema`와 100% 호환됩니다.
//! - OS별 엄격한 비공개 파일 권한(Windows ACL, Unix 0600/0700)을 적용하고 검증합니다.
//! - 토큰 및 파일 내용은 로그, 오류, 표준 출력에 절대 노출되지 않습니다.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const SCHEMA_VERSION: u32 = 1;
pub const CREDENTIALS_FILE_NAME: &str = "broker.json";
pub const MAX_CREDENTIALS_SIZE: u64 = 16_384;

/// MCP 서버용 비공개 브로커 자격 스냅샷
///
/// MCP settingsSchema:
/// ```json
/// {
///   "schemaVersion": 1,
///   "brokerUrl": "http://127.0.0.1:43127",
///   "localToken": "<secret-token>"
/// }
/// ```
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpCredentials {
    pub schema_version: u32,
    pub broker_url: String,
    pub local_token: String,
}

/// 설정 파일 경로에 대응하는 MCP 자격 파일의 절대 경로를 반환합니다.
pub fn path(config_path: &Path) -> PathBuf {
    crate::config::state_directory(config_path).join(CREDENTIALS_FILE_NAME)
}

/// 암호학적으로 안전한 64바이트(hex) 로컬 토큰을 생성합니다.
///
/// 256비트 CSPRNG 엔트로피(UUID v4 2개 결합)를 사용하며,
/// `[a-f0-9]` 문자열로 구성되어 MCP 요구 정규식 `^[A-Za-z0-9._~-]+$` 및 16~4096자 제한을 충족합니다.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// 브로커 URL 형식 검증 (`http://127.0.0.1:<port>` 형식, 슬래시/경로/쿼리/사용자정보 금지)
pub fn is_valid_broker_url(url: &str) -> bool {
    if let Some(port_str) = url.strip_prefix("http://127.0.0.1:")
        && let Ok(port) = port_str.parse::<u16>()
    {
        return port > 0;
    }
    false
}

/// 로컬 토큰 형식 검증 (16~4096자, `[A-Za-z0-9._~-]` 집합)
pub fn is_valid_local_token(token: &str) -> bool {
    token.len() >= 16
        && token.len() <= 4096
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '~' | '-'))
}

/// MCP 자격 데이터 구조 검증
pub fn validate(creds: &McpCredentials) -> Result<()> {
    ensure!(
        creds.schema_version == SCHEMA_VERSION,
        "Unsupported schemaVersion in MCP credentials"
    );
    ensure!(
        is_valid_broker_url(&creds.broker_url),
        "Invalid brokerUrl in MCP credentials; must be http://127.0.0.1:<port>"
    );
    ensure!(
        is_valid_local_token(&creds.local_token),
        "Invalid localToken format or length in MCP credentials"
    );
    Ok(())
}

/// 자격 파일 및 부모 디렉터리의 경로 안전성 검증
/// - 일반 파일 여부, 심볼릭 링크 거부, 16KiB 이하 크기 확인
/// - 부모 디렉터리 존재 및 심볼릭 링크 거부 확인
pub fn verify_path_safety(file_path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(file_path).context("Cannot inspect credential file")?;
    ensure!(
        !meta.is_symlink(),
        "Credential file must not be a symbolic link"
    );
    ensure!(meta.is_file(), "Credential path must be a regular file");
    ensure!(
        meta.len() <= MAX_CREDENTIALS_SIZE,
        "Credential file exceeds maximum size limit (16KiB)"
    );

    let parent = file_path
        .parent()
        .context("Credential file has no parent directory")?;
    let parent_meta =
        fs::symlink_metadata(parent).context("Cannot inspect credential parent directory")?;
    ensure!(
        !parent_meta.is_symlink(),
        "Credential parent directory must not be a symbolic link"
    );
    ensure!(
        parent_meta.is_dir(),
        "Credential parent path must be a directory"
    );
    Ok(())
}

#[cfg(windows)]
pub fn current_user_sid() -> Result<String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    static CACHED_SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(sid) = CACHED_SID.get() {
        return Ok(sid.clone());
    }
    crate::diag::diag("CRED_WHOAMI", "whoami /user start");
    let t0 = std::time::Instant::now();
    let mut cmd = std::process::Command::new("whoami");
    cmd.args(["/user", "/fo", "csv", "/nh"])
        .stdin(Stdio::null())
        .creation_flags(0x08000000);
    let output = cmd
        .output()
        .context("Failed to execute whoami to get user SID")?;
    crate::diag::diag(
        "CRED_WHOAMI",
        &format!(
            "whoami /user finished in {}ms, code={:?}",
            t0.elapsed().as_millis(),
            output.status.code()
        ),
    );
    let text = String::from_utf8_lossy(&output.stdout);
    for part in text.split(',') {
        let trimmed = part.trim().trim_matches('"');
        if trimmed.starts_with("S-1-") {
            let sid = trimmed.to_string();
            let _ = CACHED_SID.set(sid.clone());
            return Ok(sid);
        }
    }
    bail!("Could not parse current user SID from whoami output");
}

#[cfg(windows)]
pub fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;

    let sid = current_user_sid()?;
    let is_dir = path.is_dir();
    let perm = if is_dir { "(OI)(CI)(F)" } else { "(F)" };
    let grant = format!("*{sid}:{perm}");
    crate::diag::diag(
        "CRED_ICACLS",
        &format!("icacls set_private start: target={}", path.display()),
    );
    let t0 = std::time::Instant::now();
    let mut cmd = std::process::Command::new("icacls.exe");
    cmd.arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(&grant)
        .stdin(Stdio::null())
        .creation_flags(0x08000000);
    let output = cmd
        .output()
        .context("Failed to execute icacls to set private permissions")?;
    crate::diag::diag(
        "CRED_ICACLS",
        &format!(
            "icacls set_private finished in {}ms, code={:?}",
            t0.elapsed().as_millis(),
            output.status.code()
        ),
    );
    if !output.status.success() {
        bail!(
            "Failed to set private Windows ACL on path: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
pub fn verify_private_permissions(path: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let abs_path = std::path::absolute(path)
        .with_context(|| format!("Cannot determine absolute path for {}", path.display()))?;

    // packages/mcp-server/src/broker/settings.ts와 동일한 Windows ACL 검사 기준 적용:
    // - 파일 소유자는 현재 사용자 SID
    // - 파일은 현재 사용자, SYSTEM(S-1-5-18), BUILTIN\Administrators(S-1-5-32-544) 외 Allow 거부
    // - 직계 부모 디렉터리는 위 신뢰 주체 외 금지된 쓰기·삭제·권한 변경(852310 마스크) Allow 거부
    // - 검증 성공 시 'private' 출력, 검사 실패 시 exit 2 또는 exit 3
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$p = $env:DEVLINK_ACL_CHECK_PATH
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$trusted = @($sid, 'S-1-5-18', 'S-1-5-32-544')
foreach ($target in @($p, [System.IO.Path]::GetDirectoryName($p))) {
  $acl = if ($target -eq $p) { [System.IO.File]::GetAccessControl($target) } else { [System.IO.Directory]::GetAccessControl($target) }
  if ($target -eq $p -and $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value -ne $sid) { exit 2 }
  foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
    if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $trusted) {
      if ($target -eq $p -or (($rule.FileSystemRights -band 852310) -ne 0)) { exit 3 }
    }
  }
}
[Console]::Write('private')
"#;

    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("DEVLINK_ACL_CHECK_PATH", &abs_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(0x08000000); // CREATE_NO_WINDOW: 콘솔 창 없이 실행

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn powershell for ACL verification on {}", path.display()))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("Failed to wait for ACL verification on {}", path.display()))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "Windows ACL verification timed out on {}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    if !status.success() {
        bail!(
            "Insecure or inaccessible Windows permissions on {}",
            path.display()
        );
    }

    let mut stdout = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        use std::io::Read;
        let _ = out.read_to_end(&mut stdout);
    }

    if std::str::from_utf8(&stdout).map(|s| s.trim()) != Ok("private") {
        bail!(
            "Insecure or inaccessible Windows permissions on {}",
            path.display()
        );
    }

    Ok(())
}

#[cfg(unix)]
pub fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn verify_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let info = fs::symlink_metadata(path)?;
    let parent = path.parent().context("no parent directory")?;
    let parent_info = fs::symlink_metadata(parent)?;

    let uid = unsafe { libc::getuid() };
    if info.uid() != uid {
        bail!("Credential file is not owned by current user");
    }
    if (info.mode() & 0o077) != 0 {
        bail!("Credential file has group/other permissions");
    }
    if (parent_info.mode() & 0o022) != 0 {
        bail!("Credential parent directory is writable by group/other");
    }
    Ok(())
}

/// 비공개 임시 파일 생성 후 원자적으로 대상 경로에 교체 저장
pub fn write_atomic_private(target: &Path, creds: &McpCredentials) -> Result<()> {
    let parent = target
        .parent()
        .context("Target path has no parent directory")?;
    fs::create_dir_all(parent).context("Cannot create parent directory for credentials")?;
    set_private_permissions(parent)?;

    let json_bytes = serde_json::to_vec_pretty(creds)?;
    ensure!(
        json_bytes.len() <= MAX_CREDENTIALS_SIZE as usize,
        "Serialized MCP credentials exceed 16KiB limit"
    );

    // 같은 디렉터리에 임시 파일 생성
    let mut temp = tempfile::Builder::new()
        .prefix(".broker-")
        .tempfile_in(parent)
        .context("Cannot create temporary credential file")?;

    // 비밀값을 쓰기 전부터 비공개 권한 적용
    set_private_permissions(temp.path())?;
    temp.as_file_mut().write_all(&json_bytes)?;
    temp.as_file_mut().write_all(b"\n")?;
    temp.as_file().sync_all()?;

    // 임시 파일 권한 사전 검증
    verify_private_permissions(temp.path())?;

    // 대상 경로로 원자적 교체
    let _persisted = temp
        .persist(target)
        .map_err(|_| anyhow::anyhow!("Cannot replace MCP credential file"))?;

    // 교체 후 최종 파일 권한 재적용 및 최종 검증
    set_private_permissions(target)?;
    verify_path_safety(target)?;
    verify_private_permissions(target)?;
    Ok(())
}

/// 기존 자격 파일을 검증하고 로드합니다.
pub fn load(config_path: &Path) -> Result<McpCredentials> {
    let cred_path = path(config_path);
    if !cred_path.exists() {
        bail!("MCP credentials file does not exist");
    }
    verify_path_safety(&cred_path)?;
    verify_private_permissions(&cred_path)?;

    let mut file = File::open(&cred_path).context("Cannot open MCP credential file")?;
    let mut content = Vec::new();
    file.read_to_end(&mut content)
        .context("Cannot read MCP credential file")?;

    let creds: McpCredentials = serde_json::from_slice(&content)
        .map_err(|_| anyhow::anyhow!("Invalid MCP credential file JSON schema"))?;
    validate(&creds)?;
    Ok(creds)
}

/// 브로커 실행 시 MCP 자격 스냅샷을 발행하거나 기존 토큰을 재사용합니다.
///
/// - 유효한 기존 자격이 있으면 `local_token`을 유지하고 `broker_url`만 현재 바인딩된 포트로 갱신합니다.
/// - 자격 파일이 없으면 새로 생성합니다.
/// - 기존 파일이 손상되었거나 권한이 불안전한 경우 조용히 덮어쓰지 않고 명시적 복구 안내와 함께 오류를 반환합니다.
pub fn publish(config_path: &Path, broker_url: &str) -> Result<McpCredentials> {
    crate::diag::diag("CRED_PUB", "publish enter");
    let t_pub = std::time::Instant::now();
    let cred_path = path(config_path);
    let parent = cred_path
        .parent()
        .context("Credential path has no parent")?;
    fs::create_dir_all(parent)?;
    set_private_permissions(parent)?;

    let local_token = if cred_path.exists() {
        match load(config_path) {
            Ok(existing) => {
                if existing.broker_url == broker_url {
                    crate::diag::diag(
                        "CRED_PUB",
                        &format!("publish reuse existing in {}ms", t_pub.elapsed().as_millis()),
                    );
                    return Ok(existing);
                }
                existing.local_token
            }
            Err(_) => {
                bail!(
                    "Existing MCP credential file at '{}' is invalid or has insecure permissions; \
                     use 'devlink mcp rotate' or remove the file manually to recover",
                    cred_path.display()
                );
            }
        }
    } else {
        generate_token()
    };

    let creds = McpCredentials {
        schema_version: SCHEMA_VERSION,
        broker_url: broker_url.to_string(),
        local_token,
    };
    validate(&creds)?;
    write_atomic_private(&cred_path, &creds)?;
    crate::diag::diag(
        "CRED_PUB",
        &format!("publish complete in {}ms", t_pub.elapsed().as_millis()),
    );
    Ok(creds)
}

/// 브로커가 중지된 상태에서만 수동으로 local_token을 회전(재발급)합니다.
///
/// 브로커가 실행 중인 경우에는 상태 일관성을 위해 회전을 거부합니다.
pub fn rotate(config_path: &Path) -> Result<McpCredentials> {
    let state_dir = crate::config::state_directory(config_path);
    fs::create_dir_all(&state_dir)?;

    // start/stop과 충돌을 방지하기 위해 control.lock 획득
    let _control_guard = crate::config::lock(&state_dir.join("control.lock"))?;

    // 브로커 실행 여부 검사 (실행 중이면 회전 거부)
    if crate::local::running(config_path)? {
        bail!("Cannot rotate token while broker is running; stop the broker first");
    }

    // 이중 확인을 위한 broker.lock 획득 시도
    let _broker_guard = crate::config::lock(&state_dir.join("broker.lock"))?;

    let cred_path = path(config_path);
    let broker_url = if cred_path.exists() {
        if let Ok(content) = fs::read(&cred_path) {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&content) {
                if let Some(url) = val.get("brokerUrl").and_then(|u| u.as_str()) {
                    if is_valid_broker_url(url) {
                        url.to_string()
                    } else {
                        default_broker_url(config_path)?
                    }
                } else {
                    default_broker_url(config_path)?
                }
            } else {
                default_broker_url(config_path)?
            }
        } else {
            default_broker_url(config_path)?
        }
    } else {
        default_broker_url(config_path)?
    };

    let new_creds = McpCredentials {
        schema_version: SCHEMA_VERSION,
        broker_url,
        local_token: generate_token(),
    };
    validate(&new_creds)?;
    write_atomic_private(&cred_path, &new_creds)?;
    Ok(new_creds)
}

fn default_broker_url(config_path: &Path) -> Result<String> {
    let config = crate::config::Config::load(config_path).unwrap_or_default();
    let port = if config.local_port == 0 {
        43127
    } else {
        config.local_port
    };
    Ok(format!("http://127.0.0.1:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        let broker_url = "http://127.0.0.1:43127";
        let creds = publish(&config_path, broker_url).unwrap();
        assert_eq!(creds.schema_version, 1);
        assert_eq!(creds.broker_url, broker_url);
        let loaded = load(&config_path).unwrap();
        assert_eq!(creds, loaded);
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_verify_private_permissions_three_cases() {
        // Case 1: 정상 비공개 파일·부모 → 허용
        let dir1 = tempfile::tempdir().unwrap();
        let file1 = dir1.path().join("broker.json");
        std::fs::write(&file1, b"test").unwrap();
        set_private_permissions(dir1.path()).unwrap();
        set_private_permissions(&file1).unwrap();
        let res1 = verify_private_permissions(&file1);
        println!("Case 1 Result: {:?}", res1);
        assert!(res1.is_ok(), "Case 1 failed: {:?}", res1.err());

        // Case 2: 파일에 Everyone 읽기 Allow 추가 → 거부
        let status2 = std::process::Command::new("icacls.exe")
            .arg(&file1)
            .arg("/grant")
            .arg("*S-1-1-0:(R)")
            .status()
            .unwrap();
        assert!(status2.success());
        let res2 = verify_private_permissions(&file1);
        println!("Case 2 Result: {:?}", res2);
        assert!(res2.is_err(), "Case 2 expected error, got Ok");

        // Case 3: 부모에 Everyone 쓰기 Allow 추가 → 거부
        let dir3 = tempfile::tempdir().unwrap();
        let file3 = dir3.path().join("broker.json");
        std::fs::write(&file3, b"test").unwrap();
        set_private_permissions(dir3.path()).unwrap();
        set_private_permissions(&file3).unwrap();
        assert!(verify_private_permissions(&file3).is_ok());

        let status3 = std::process::Command::new("icacls.exe")
            .arg(dir3.path())
            .arg("/grant")
            .arg("*S-1-1-0:(W)")
            .status()
            .unwrap();
        assert!(status3.success());
        let res3 = verify_private_permissions(&file3);
        println!("Case 3 Result: {:?}", res3);
        assert!(res3.is_err(), "Case 3 expected error, got Ok");
    }
}

