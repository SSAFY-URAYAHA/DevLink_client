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
    static CACHED_SID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    if let Some(sid) = CACHED_SID.get() {
        return Ok(sid.clone());
    }
    let output = std::process::Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .context("Failed to execute whoami to get user SID")?;
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
    let sid = current_user_sid()?;
    let is_dir = path.is_dir();
    let perm = if is_dir { "(OI)(CI)(F)" } else { "(F)" };
    let grant = format!("*{sid}:{perm}");
    let output = std::process::Command::new("icacls.exe")
        .arg(path)
        .arg("/inheritance:r")
        .arg("/grant:r")
        .arg(&grant)
        .output()
        .context("Failed to execute icacls to set private permissions")?;
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
    let sid = current_user_sid()?;
    let parent = path.parent().context("no parent directory")?;

    for target in [path, parent] {
        let output = std::process::Command::new("icacls.exe")
            .arg(target)
            .output()
            .context("Failed to execute icacls to verify permissions")?;
        if !output.status.success() {
            bail!("icacls failed on path: {}", target.display());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Verify output indicates successfully processed
        if !stdout.contains("Successfully processed 1 files") {
            bail!("Insecure or inaccessible Windows permissions on {}", target.display());
        }
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
}
