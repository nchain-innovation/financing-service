use actix_web::{http::header::ContentType, HttpResponse};
use serde::Serialize;

#[derive(Debug, Serialize, Clone, Copy)]
pub enum BlockchainConnectionStatus {
    Unknown,
    Failed,
    Connected,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub description: String,
}

#[derive(Serialize)]
pub struct SuccessResponse {
    pub status: &'static str,
}

impl SuccessResponse {
    pub fn new() -> Self {
        Self { status: "Success" }
    }
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub version: String,
    pub blockchain_status: BlockchainConnectionStatus,
    pub blockchain_update_time: String,
}

#[derive(Serialize)]
pub struct AddressResponse {
    pub address: String,
}

#[derive(Serialize)]
pub struct BalanceResponse {
    pub confirmed: i64,
    pub unconfirmed: i64,
}

#[derive(Serialize)]
pub struct OutpointResponse {
    pub hash: String,
    pub index: u32,
}

#[derive(Serialize)]
pub struct TxResponse {
    pub tx: String,
}

#[derive(Serialize)]
pub struct FundingResponseJson {
    pub outpoints: Vec<OutpointResponse>,
    pub txs: Vec<TxResponse>,
}

pub fn error_response(description: impl Into<String>) -> HttpResponse {
    HttpResponse::UnprocessableEntity()
        .content_type(ContentType::json())
        .json(ErrorResponse {
            description: description.into(),
        })
}

pub fn json_ok<T: Serialize>(value: &T) -> HttpResponse {
    HttpResponse::Ok()
        .content_type(ContentType::json())
        .json(value)
}
