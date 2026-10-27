//! # DevLink Local Broker
//!
//! 로컬 환경에서 백그라운드로 실행되는 핵심 브로커 데몬 모듈입니다.
//! - 로컬 제어용 HTTP 서버(Axum)를 구동하여 상태 조회(health) 및 정상 종료(shutdown) 요청을 처리합니다.
//! - 중앙 웹소켓 서버(central)와의 연결 세션을 백그라운드 태스크로 관리합니다.

use crate::{
    central::{self, ConnectionStatus, SharedStatus},
    config::{self, Config, directory},
    local::{self, API_VERSION, Health, Runtime, SERVICE},
};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::RwLock, time::timeout};
use tokio_util::sync::CancellationToken;

/// 로컬 HTTP API 핸들러에서 공유하는 상태
#[derive(Clone)]
struct ApiState {
    /// 브로커 런타임 정보 (PID, 포트, 인증 토큰 등)
    runtime: Arc<Runtime>,
    /// 중앙 서버와의 웹소켓 연결 상태
    status: SharedStatus,
    /// 프로세스 전체에 종료 시그널을 전파하는 토큰
    cancel: CancellationToken,
}

/// 브로커 종료 시 런타임 파일(runtime.json)을 자동으로 정리(삭제)하는 RAII 가드
struct RuntimeGuard(std::path::PathBuf);
impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// 브로커의 메인 실행 루프
/// - 설정 로드 및 환경변수(DEVLINK_TOKEN) 검증
/// - 로컬 루프백 TCP 리스너 바인딩 및 런타임 메타데이터 등록
/// - 중앙 서버 연동 태스크 실행 및 Axum 기반 제어 HTTP 서버 구동
pub async fn run(path: &Path) -> Result<()> {
    // 설정 로드 및 환경변수 토큰 확인
    let config = Config::load(path)?;
    let token = central_token()?;

    // 중복 실행 방지를 위한 파일 락 획득 (broker.lock)
    let _lock = config::lock(&directory(path).join("broker.lock"))?;

    // 로컬 제어용 루프백(127.0.0.1) TCP 포트 바인딩
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.local_port))
        .await
        .context("Cannot bind broker loopback port; check for a port conflict")?;

    // 현재 실행 인스턴스 정보 생성 및 파일로 게시(publish)
    let runtime = Arc::new(Runtime {
        api_version: API_VERSION,
        pid: std::process::id(),
        port: listener.local_addr()?.port(),
        instance_id: uuid::Uuid::new_v4().to_string(),
        control_token: uuid::Uuid::new_v4().to_string(),
    });
    local::publish(path, &runtime)?;
    let _runtime_guard = RuntimeGuard(Runtime::path(path));

    // 취소 토큰 및 중앙 서버 연결 상태 초기화
    let cancel = CancellationToken::new();
    let status = Arc::new(RwLock::new(ConnectionStatus::Connecting));

    // 중앙 서버와 통신하는 비동기 태스크 시작
    let central = tokio::spawn(central::run(config, token, status.clone(), cancel.clone()));

    // 로컬 제어 API 라우터 설정 (/health, /shutdown)
    let app = Router::new()
        .route("/health", get(health))
        .route("/shutdown", post(shutdown))
        .fallback(not_found)
        .with_state(ApiState {
            runtime,
            status,
            cancel: cancel.clone(),
        });

    // Graceful Shutdown 처리 (취소 토큰 수신 또는 OS 종료 시그널 감지 시)
    let shutdown_token = cancel.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        tokio::select! {
            _ = shutdown_token.cancelled() => {},
            _ = termination_signal() => shutdown_token.cancel()
        }
    });

    // 클라이언트가 HTTP 연결을 계속 유지하더라도 안전하게 타임아웃(3초) 내에 종료되도록 제한
    let server = std::future::IntoFuture::into_future(server);
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result,
        _ = cancel.cancelled() => {
            match timeout(Duration::from_secs(3), &mut server).await {
                Ok(result) => result,
                Err(_) => Ok(()),
            }
        }
    };

    // 중앙 서버 태스크 종료 요청 및 대기 (최대 2초 후 강제 취소)
    cancel.cancel();
    let abort = central.abort_handle();
    if timeout(Duration::from_secs(2), central).await.is_err() {
        abort.abort();
    }
    result.context("Broker HTTP server failed")
}

/// 환경변수 `DEVLINK_TOKEN`에서 중앙 서버 인증 토큰을 읽어와 유효성을 검증합니다.
pub fn central_token() -> Result<String> {
    let token = std::env::var("DEVLINK_TOKEN")
        .map_err(|_| anyhow::anyhow!("Set DEVLINK_TOKEN before starting the broker"))?;
    anyhow::ensure!(
        !token.trim().is_empty() && token.len() <= 8192,
        "DEVLINK_TOKEN must be nonempty and at most 8192 bytes"
    );
    Ok(token)
}

/// 로컬 HTTP 제어 요청의 Authorization 헤더와 Origin을 검증합니다.
/// - 웹 브라우저에서의 무단 요청(CSRF 등)을 차단하기 위해 `Origin` 헤더가 있으면 거부합니다.
/// - 런타임에 발급된 일회성 control_token과 Bearer 토큰이 일치해야 합니다.
fn authorized(headers: &HeaderMap, state: &ApiState) -> bool {
    if headers.contains_key("origin") {
        return false;
    }
    headers
        .get("authorization")
        .and_then(|header| header.to_str().ok())
        .is_some_and(|header| {
            header.strip_prefix("Bearer ") == Some(state.runtime.control_token.as_str())
        })
}

/// 표준 API 에러 응답 JSON을 생성합니다.
fn error(code: StatusCode, message: &'static str) -> Response {
    (code, Json(serde_json::json!({ "apiVersion": API_VERSION, "error": { "code": code.as_u16(), "message": message } }))).into_response()
}

/// `GET /health` 핸들러: 브로커의 프로세스 정보 및 중앙 서버 연결 상태 반환
async fn health(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    Json(Health {
        service: SERVICE.into(),
        api_version: API_VERSION,
        pid: state.runtime.pid,
        instance_id: state.runtime.instance_id.clone(),
        central_server: *state.status.read().await,
    })
    .into_response()
}

/// `POST /shutdown` 핸들러: 브로커 정상 종료 요청 처리
async fn shutdown(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    state.cancel.cancel();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "apiVersion": API_VERSION, "status": "stopping" })),
    )
        .into_response()
}

/// 등록되지 않은 엔드포인트 요청에 대한 404 핸들러
async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "Unknown endpoint")
}

/// OS 레벨의 프로세스 종료 시그널(Ctrl+C 또는 SIGTERM)을 대기합니다.
async fn termination_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
