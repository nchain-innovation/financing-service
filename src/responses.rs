use actix_web::{http::header::ContentType, http::StatusCode, HttpResponse};
use serde::Serialize;

#[derive(Debug, Serialize, Clone, Copy)]
pub enum BlockchainConnectionStatus {
    Unknown,
    Failed,
    Connected,
}

/// Stable, machine-readable classification of an error response.
///
/// The `description` accompanying a code is prose for humans and may be
/// reworded at any time; the code is the contract. Clients should treat an
/// unrecognised code the way they treated responses before codes existed.
#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The wallet's total balance cannot cover the request. Retryable once
    /// the wallet is topped up.
    InsufficientBalance,
    /// The balance is sufficient in total, but no combination of UTXOs can
    /// satisfy the request. Retryable once the wallet's UTXOs are split.
    NoSuitableUtxo,
    /// No such `client_id`. Never retryable without a configuration change.
    UnknownClient,
    /// The `client_id` is already configured.
    ClientExists,
    /// The request itself is malformed. Never retryable unchanged.
    InvalidRequest,
    /// The funding transaction was built and signed but the node rejected it
    /// or was unreachable. The client's own retry may succeed.
    BroadcastFailed,
    /// Some of the requested funding transactions broadcast and some did not.
    /// The successful ones are in the response body.
    PartialBroadcast,
    /// The blockchain interface could not be reached to refresh chain state.
    /// Retryable.
    ChainUnavailable,
    /// An unexpected internal failure.
    Internal,
    /// Authentication missing or invalid.
    Unauthorized,
    /// The caller exceeded the configured request rate. Retryable after the
    /// interval given in the description.
    RateLimited,
    /// A request carrying this `idempotency_key` is still being processed.
    /// Retry shortly; the completed response will then be replayed.
    KeyInProgress,
    /// This `idempotency_key` was already used with a materially different
    /// request. Use a fresh `idempotency_key`.
    IdempotencyKeyReused,
}

impl ErrorCode {
    /// HTTP status this code is reported with.
    ///
    /// Splitting the statuses lets a caller classify a failure without
    /// parsing the body, which matters for anything between the caller and
    /// this service -- a proxy, a gateway, a retry policy, an alerting rule --
    /// that sees the status and not the payload. The rule is that `5xx` is
    /// worth retrying unchanged and `4xx` needs the caller or an operator to
    /// change something.
    pub fn status(self) -> StatusCode {
        match self {
            // the caller must change the request
            ErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
            // the named client does not exist
            ErrorCode::UnknownClient => StatusCode::NOT_FOUND,
            // the request is well formed but conflicts with current state
            ErrorCode::ClientExists
            | ErrorCode::InsufficientBalance
            | ErrorCode::NoSuitableUtxo
            | ErrorCode::KeyInProgress
            | ErrorCode::IdempotencyKeyReused => StatusCode::CONFLICT,
            // upstream node rejected the transaction or was unreachable
            ErrorCode::BroadcastFailed => StatusCode::BAD_GATEWAY,
            ErrorCode::ChainUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            // Neither a success nor a clean failure: some transactions are on
            // chain and the caller has to read the body to learn which. Kept
            // as 422 deliberately -- 207 Multi-Status is the honest code but
            // is WebDAV and rarely handled, and any plain failure status would
            // invite a caller to ignore a body it must not ignore.
            ErrorCode::PartialBroadcast => StatusCode::UNPROCESSABLE_ENTITY,
        }
    }
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub description: String,
}

/// An error carrying both its stable code and its human-readable description.
///
/// Threading this through the service layer keeps the code attached to the
/// condition that produced it, rather than re-deriving it by matching on the
/// description at the response boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodedError {
    pub code: ErrorCode,
    pub description: String,
}

impl CodedError {
    pub fn new(code: ErrorCode, description: impl Into<String>) -> Self {
        Self {
            code,
            description: description.into(),
        }
    }

    pub fn internal(description: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, description)
    }
}

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.description)
    }
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

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct OutpointResponse {
    pub hash: String,
    pub index: u32,
    /// Value of this output, read from the broadcast transaction.
    pub satoshi: i64,
    /// Locking script of this output as hex, read from the broadcast
    /// transaction.
    pub locking_script: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct TxResponse {
    pub tx: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FundingResponseJson {
    pub outpoints: Vec<OutpointResponse>,
    pub txs: Vec<TxResponse>,
    /// True when this response is being replayed against a previously used
    /// `idempotency_key` rather than describing a fresh funding.
    ///
    /// Serialised only when true, so an ordinary funding response is
    /// unchanged; clients should read its absence as false. A client that
    /// expected fresh funds and receives `"replayed": true` is holding
    /// outpoints it has already been given, and very likely already spent --
    /// better to fail on that immediately than to sign over a spent output
    /// and get an opaque script error from the node.
    #[serde(skip_serializing_if = "is_false")]
    pub replayed: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Serialize)]
pub struct PartialFundingErrorResponse {
    pub code: ErrorCode,
    pub description: String,
    pub outpoints: Vec<OutpointResponse>,
    pub txs: Vec<TxResponse>,
    /// As [`FundingResponseJson::replayed`]. A replayed partial batch matters
    /// just as much: those transactions are on chain from the first call.
    #[serde(skip_serializing_if = "is_false")]
    pub replayed: bool,
}

pub fn partial_funding_error_response(
    description: impl Into<String>,
    funding: &FundingResponseJson,
) -> HttpResponse {
    HttpResponse::build(ErrorCode::PartialBroadcast.status())
        .content_type(ContentType::json())
        .json(PartialFundingErrorResponse {
            code: ErrorCode::PartialBroadcast,
            description: description.into(),
            outpoints: funding.outpoints.clone(),
            txs: funding.txs.clone(),
            replayed: funding.replayed,
        })
}

pub fn error_response(code: ErrorCode, description: impl Into<String>) -> HttpResponse {
    error_response_with_status(code.status(), code, description)
}

/// Build an error response with a status other than the code's default.
///
/// Needed only for `UnknownClient`, which is a missing resource on the
/// `/client/{client_id}/...` routes but a malformed body on `POST /fund`.
pub fn error_response_with_status(
    status: StatusCode,
    code: ErrorCode,
    description: impl Into<String>,
) -> HttpResponse {
    HttpResponse::build(status)
        .content_type(ContentType::json())
        .json(ErrorResponse {
            code,
            description: description.into(),
        })
}

/// Build an error response from an error that already carries its code.
pub fn coded_error_response(error: CodedError) -> HttpResponse {
    error_response(error.code, error.description)
}

pub fn unauthorized_response() -> HttpResponse {
    HttpResponse::build(ErrorCode::Unauthorized.status())
        .content_type(ContentType::json())
        .json(ErrorResponse {
            code: ErrorCode::Unauthorized,
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

    /// The serialised form of each code is the API contract. If a variant is
    /// renamed, this test fails rather than silently changing what clients see.
    #[test]
    fn error_codes_serialise_to_their_documented_strings() {
        let cases = [
            (ErrorCode::InsufficientBalance, "insufficient_balance"),
            (ErrorCode::NoSuitableUtxo, "no_suitable_utxo"),
            (ErrorCode::UnknownClient, "unknown_client"),
            (ErrorCode::ClientExists, "client_exists"),
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::BroadcastFailed, "broadcast_failed"),
            (ErrorCode::PartialBroadcast, "partial_broadcast"),
            (ErrorCode::ChainUnavailable, "chain_unavailable"),
            (ErrorCode::Internal, "internal"),
            (ErrorCode::Unauthorized, "unauthorized"),
            (ErrorCode::RateLimited, "rate_limited"),
            (ErrorCode::KeyInProgress, "key_in_progress"),
            (ErrorCode::IdempotencyKeyReused, "idempotency_key_reused"),
        ];
        for (code, expected) in cases {
            assert_eq!(
                serde_json::to_value(code).unwrap(),
                serde_json::Value::String(expected.to_string()),
            );
        }
    }

    #[test]
    fn error_response_carries_code_and_description() {
        let response = error_response(ErrorCode::UnknownClient, "Unknown client_id id9");
        assert_eq!(response.status(), actix_http::StatusCode::NOT_FOUND);
    }

    /// The status for each code is part of the API, so pin the whole mapping.
    /// A caller may branch on the status without reading the body, and a
    /// change here would break that silently.
    #[test]
    fn each_error_code_maps_to_its_documented_status() {
        use actix_http::StatusCode as S;
        let cases = [
            (ErrorCode::InvalidRequest, S::BAD_REQUEST),
            (ErrorCode::UnknownClient, S::NOT_FOUND),
            (ErrorCode::ClientExists, S::CONFLICT),
            (ErrorCode::InsufficientBalance, S::CONFLICT),
            (ErrorCode::NoSuitableUtxo, S::CONFLICT),
            (ErrorCode::KeyInProgress, S::CONFLICT),
            (ErrorCode::IdempotencyKeyReused, S::CONFLICT),
            (ErrorCode::BroadcastFailed, S::BAD_GATEWAY),
            (ErrorCode::ChainUnavailable, S::SERVICE_UNAVAILABLE),
            (ErrorCode::Internal, S::INTERNAL_SERVER_ERROR),
            (ErrorCode::Unauthorized, S::UNAUTHORIZED),
            (ErrorCode::RateLimited, S::TOO_MANY_REQUESTS),
            (ErrorCode::PartialBroadcast, S::UNPROCESSABLE_ENTITY),
        ];
        for (code, expected) in cases {
            assert_eq!(code.status(), expected, "status for {code:?}");
        }
    }

    /// Retry semantics fall out of the split: 5xx is worth retrying
    /// unchanged, 4xx needs the caller or an operator to act.
    #[test]
    fn retryable_codes_are_5xx_and_client_errors_are_4xx() {
        for code in [ErrorCode::BroadcastFailed, ErrorCode::ChainUnavailable] {
            assert!(code.status().is_server_error(), "{code:?} should be 5xx");
        }
        for code in [
            ErrorCode::InvalidRequest,
            ErrorCode::UnknownClient,
            ErrorCode::ClientExists,
            ErrorCode::IdempotencyKeyReused,
        ] {
            assert!(code.status().is_client_error(), "{code:?} should be 4xx");
        }
    }

    #[test]
    fn coded_error_response_preserves_the_originating_code() {
        let error = CodedError::new(ErrorCode::BroadcastFailed, "node unreachable");
        assert_eq!(error.code, ErrorCode::BroadcastFailed);
        assert_eq!(error.to_string(), "node unreachable");
        let response = coded_error_response(error);
        assert_eq!(response.status(), actix_http::StatusCode::BAD_GATEWAY);
    }

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
            replayed: false,
        };
        let response = partial_funding_error_response("partial failure", &funding);
        assert_eq!(
            response.status(),
            actix_http::StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
