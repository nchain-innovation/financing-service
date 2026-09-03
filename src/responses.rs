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
pub struct HealthResponse {
    pub status: &'static str,
}

impl HealthResponse {
    pub fn ok() -> Self {
        Self { status: "ok" }
    }
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

#[derive(Serialize, Clone)]
pub struct OutpointResponse {
    pub hash: String,
    pub index: u32,
    /// Value of this output, read from the broadcast transaction.
    pub satoshi: i64,
    /// Locking script of this output as hex, read from the broadcast
    /// transaction.
    pub locking_script: String,
}

#[derive(Serialize, Clone)]
pub struct TxResponse {
    pub tx: String,
}

#[derive(Serialize, Clone)]
pub struct FundingResponseJson {
    pub outpoints: Vec<OutpointResponse>,
    pub txs: Vec<TxResponse>,
}

#[derive(Serialize)]
pub struct PartialFundingErrorResponse {
    pub description: String,
    pub outpoints: Vec<OutpointResponse>,
    pub txs: Vec<TxResponse>,
}

pub fn partial_funding_error_response(
    description: impl Into<String>,
    funding: &FundingResponseJson,
) -> HttpResponse {
    HttpResponse::UnprocessableEntity()
        .content_type(ContentType::json())
        .json(PartialFundingErrorResponse {
            description: description.into(),
            outpoints: funding.outpoints.clone(),
            txs: funding.txs.clone(),
        })
}

pub fn error_response(description: impl Into<String>) -> HttpResponse {
    HttpResponse::UnprocessableEntity()
        .content_type(ContentType::json())
        .json(ErrorResponse {
            description: description.into(),
        })
}

pub fn unauthorized_response() -> HttpResponse {
    HttpResponse::Unauthorized()
        .content_type(ContentType::json())
        .json(ErrorResponse {
            description: "Unauthorized".to_string(),
        })
}

pub fn json_ok<T: Serialize>(value: &T) -> HttpResponse {
    HttpResponse::Ok()
        .content_type(ContentType::json())
        .json(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_funding_error_response_includes_successful_transactions() {
        let funding = FundingResponseJson {
            outpoints: vec![OutpointResponse {
                hash: "abc123".to_string(),
                index: 1,
                satoshi: 123,
                locking_script: "76a914ddc574807c3035ab43553a22c0b9df1f55737fae88ac".to_string(),
            }],
            txs: vec![TxResponse {
                tx: "010000".to_string(),
            }],
        };
        let response = partial_funding_error_response("partial failure", &funding);
        assert_eq!(
            response.status(),
            actix_http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
