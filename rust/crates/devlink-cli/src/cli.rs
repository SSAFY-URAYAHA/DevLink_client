use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use devlink_broker::{
    broker,
    config::{self, Config, directory},
    local::{self, Runtime},
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tokio::time::{Instant, sleep};

#[derive(Parser)]
#[command(name = "devlink", version, about = "DevLink CLI and local broker")]
pub struct Cli {
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Commands,
}
#[derive(Subcommand)]
pub enum Commands {
    /// Create a configuration file without overwriting existing settings
    Init,
    /// Start an independent background broker
    Start,
    /// Gracefully stop the broker
    Stop,
    /// Show local broker and central WebSocket state
    Status,
    /// Read or update non-secret settings (changes apply after restart)
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommand>,
    },
    /// Internal command: run the broker loop in foreground (spawned by start)
    #[command(hide = true)]
    Broker,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show the current configuration formatted as JSON
    Show,
    /// Update a configuration key-value pair
    Set {
        /// Configuration key to update (e.g. serverUrl, agent, localPort)
        key: String,
        /// New value for the configuration key
        value: String,
    },
}

/// CLI 진입점: 파싱된 CLI 명령어에 따라 적절한 로직을 라우팅하여 실행합니다.
pub async fn run(cli: Cli) -> Result<()> {
    let path = config::config_path(cli.config)?;
    match cli.command {
        // 설정 파일 초기화 (기존 파일이 없을 때만 기본 생성)
        Commands::Init => {
            println!(
                "{}",
                if config::init(&path)? {
                    "Configuration created"
                } else {
                    "Configuration already exists"
                }
            );
        }
        // 설정 조회 및 업데이트
        Commands::Config { command } => match command {
            // 특정 설정 키-값 수정
            Some(ConfigCommand::Set { key, value }) => {
                config::set(&path, &key, &value)?;
                println!("Configuration updated; restart broker to apply");
            }
            // 전체 설정 JSON 형식 출력 (기본값)
            None | Some(ConfigCommand::Show) => {
                println!("{}", serde_json::to_string_pretty(&Config::load(&path)?)?)
            }
        },
        // 로컬 브로커 백그라운드 프로세스 시작
        Commands::Start => start(&path).await?,
        // 로컬 브로커 실행 여부 및 중앙 서버(WebSocket) 연결 상태 확인
        Commands::Status => {
            if !local::running(&path)? {
                println!("Local Broker  : Stopped\nCentral Server: Disconnected");
            } else {
                let health = Runtime::load(&path)?.health().await?;
                println!(
                    "Local Broker  : Running (PID: {})\nCentral Server: {:?}",
                    health.pid, health.central_server
                );
            }
        }
        // 실행 중인 브로커 정상 종료 (Graceful shutdown)
        Commands::Stop => {
            if !directory(&path).exists() {
                println!("Broker already stopped");
                return Ok(());
            }
            // 중복 제어 방지를 위한 파일 락 획득
            let _guard = config::lock(&directory(&path).join("control.lock"))?;
            if !local::running(&path)? {
                println!("Broker already stopped");
                return Ok(());
            }
            // 종료 요청 전송 후 최대 8초간 정상 종료 대기
            Runtime::load(&path)?.stop().await?;
            let deadline = Instant::now() + Duration::from_secs(8);
            while local::running(&path)? {
                if Instant::now() >= deadline {
                    bail!("Broker shutdown timed out; process was not force-killed");
                }
                sleep(Duration::from_millis(50)).await;
            }
            println!("Broker stopped");
        }
        // 포그라운드에서 직접 브로커 통신 루프 실행 (start 명령어로 생성된 자식 프로세스가 실행)
        Commands::Broker => broker::run(&path).await?,
    }
    Ok(())
}

/// 브로커를 터미널과 분리된 백그라운드 독립 프로세스로 구동하고 정상 기동을 검증합니다.
async fn start(path: &Path) -> Result<()> {
    Config::load(path)?;
    let _guard = config::lock(&directory(path).join("control.lock"))?;
    if local::running(path)? {
        Runtime::load(path)?.health().await?;
        println!("Broker already running");
        return Ok(());
    }
    broker::central_token()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--config")
        .arg(path)
        .arg("broker")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Windows 파이프 핸들 상속 방지
        prevent_stdio_inheritance()?;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: 콘솔 창 없는 독립 프로세스 그룹으로 분리
        command.creation_flags(0x00000008 | 0x00000200);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().context("Cannot spawn broker process")?;
    let deadline = Instant::now() + Duration::from_secs(8);
    // 최대 8초 동안 프로세스 조기 종료 여부와 헬스체크 응답을 50ms 주기로 폴링
    loop {
        if let Some(_exit) = child.try_wait()? {
            bail!(
                "Broker exited during startup; check configuration and local port, or run devlink broker in foreground"
            );
        }
        if let Ok(runtime) = Runtime::load(path)
            && runtime.pid == child.id()
            && runtime.health().await.is_ok()
        {
            println!("Broker started (PID: {})", child.id());
            return Ok(());
        }
        if Instant::now() >= deadline {
            // 타임아웃 발생 시 생성한 자식 프로세스 정리
            let _ = child.kill();
            let _ = child.wait();
            bail!("Broker startup timed out; child process cleaned up");
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// Windows 환경에서 부모 CLI의 표준 핸들(stdin, stdout, stderr) 상속을 차단합니다.
/// 백그라운드 브로커가 파이프를 잡고 있어 CLI 종료 후에도 호출자가 대기(Hang)하는 문제를 방지합니다.
#[cfg(windows)]
fn prevent_stdio_inheritance() -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    // std::process on Windows inherits all inheritable handles, including the CLI's
    // original capture pipes even when the broker's own stdio is redirected to NUL.
    // Clear inheritance on those three existing handles so `devlink start` captured
    // by an Agent/test can reach EOF while its background broker stays alive.
    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle borrows a process-owned handle; it is never closed here.
        let handle = unsafe { GetStdHandle(stream) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: This only changes the inheritance flag on a valid borrowed handle.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            bail!("Cannot prevent CLI standard handle inheritance");
        }
    }
    Ok(())
}
