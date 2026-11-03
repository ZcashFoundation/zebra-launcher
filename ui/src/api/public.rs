//! The public services' commands and event (docs/PUBLIC-SERVICES.md), to Tauri or to the mock.

use launcher_types::{
    Network, NodeId, events,
    public::{
        ConnectionReport, DnsProvider, OpenError, PublicService, PublicStatus, StepAnswer, StepId,
        StillPublic,
    },
};
use serde::Serialize;

use super::{ApiError, ApiResult, mock_handler, wire};
use crate::{
    mock,
    tauri::{self, is_tauri},
};

pub async fn get_public_status() -> ApiResult<Vec<PublicStatus>> {
    if !is_tauri() {
        return Ok(wire(mock::public::get_public_status().await)?);
    }
    Ok(tauri::invoke0("get_public_status").await?)
}

/// Calls `on_status` with every changed service status.
pub async fn on_public_status(
    on_status: impl Fn(PublicStatus) + 'static,
    on_error: impl Fn(String) + 'static,
) -> ApiResult<()> {
    if !is_tauri() {
        mock::public::on_public_status(mock_handler("`public_status` event", on_status, on_error));
        return Ok(());
    }
    Ok(tauri::listen(events::PUBLIC_STATUS, on_status, on_error).await?)
}

pub async fn check_connection() -> ApiResult<ConnectionReport> {
    if !is_tauri() {
        return Ok(wire(
            mock::public::check_connection().await.map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke0("check_connection").await?)
}

#[derive(Serialize)]
struct BeginArgs {
    service: PublicService,
    network: Network,
}

pub async fn begin_public_setup(network: Network) -> ApiResult<NodeId> {
    if !is_tauri() {
        return Ok(wire(
            mock::public::begin_public_setup(network)
                .await
                .map_err(ApiError)?,
        )?);
    }
    let args = BeginArgs {
        service: PublicService::LightServer,
        network,
    };
    Ok(tauri::invoke("begin_public_setup", &args).await?)
}

#[derive(Serialize)]
struct SwitchArgs {
    on: bool,
}

pub async fn set_public_switch(on: bool) -> ApiResult<()> {
    if !is_tauri() {
        return mock::public::set_public_switch(on).await.map_err(ApiError);
    }
    Ok(tauri::invoke("set_public_switch", &SwitchArgs { on }).await?)
}

#[derive(Serialize)]
struct AnswerArgs {
    answer: StepAnswer,
}

pub async fn answer_public_step(answer: StepAnswer) -> ApiResult<()> {
    if !is_tauri() {
        return mock::public::answer_public_step(wire(answer)?)
            .await
            .map_err(ApiError);
    }
    Ok(tauri::invoke("answer_public_step", &AnswerArgs { answer }).await?)
}

#[derive(Serialize)]
struct StepArgs {
    step: StepId,
}

pub async fn run_public_checks(step: StepId) -> ApiResult<()> {
    if !is_tauri() {
        return mock::public::run_public_checks(step)
            .await
            .map_err(ApiError);
    }
    Ok(tauri::invoke("run_public_checks", &StepArgs { step }).await?)
}

/// "Open to the internet". Refused with the failing check unless every gate passes.
pub async fn open_public_service() -> Result<(), OpenError> {
    let refused = |error: ApiError| OpenError {
        check: None,
        message: error.0,
    };
    if !is_tauri() {
        return mock::public::open_public_service().await;
    }
    match tauri::invoke0::<()>("open_public_service").await {
        Ok(()) => Ok(()),
        Err(error) => Err(tauri::from_js(error.clone()).unwrap_or_else(|_| refused(error.into()))),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ForgetArgs {
    remove_token: bool,
}

pub async fn forget_public_service(remove_token: bool) -> ApiResult<StillPublic> {
    if !is_tauri() {
        return Ok(wire(
            mock::public::forget_public_service(remove_token)
                .await
                .map_err(ApiError)?,
        )?);
    }
    Ok(tauri::invoke("forget_public_service", &ForgetArgs { remove_token }).await?)
}

#[derive(Serialize)]
struct SignupArgs {
    provider: DnsProvider,
}

/// Opens the provider's sign-up page, compiled into the backend.
pub async fn open_signup(provider: DnsProvider) -> ApiResult<()> {
    if !is_tauri() {
        return mock::public::open_signup(provider).await.map_err(ApiError);
    }
    Ok(tauri::invoke("open_signup", &SignupArgs { provider }).await?)
}
