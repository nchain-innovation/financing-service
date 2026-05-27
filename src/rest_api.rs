use actix_web::{delete, get, post, web, HttpRequest, HttpResponse, Responder};
use log::debug;
use serde::Deserialize;
use std::sync::Arc;

use crate::{
    auth::{authorize_admin, authorize_client},
    client::FundRequest,
    config::ClientConfig,
    responses::{
        error_response, json_ok, AddressResponse, BalanceResponse, HealthResponse, SuccessResponse,
    },
    secrets::{secret_reference, validate_env_var_name},
    service::Service,
};

#[derive(Deserialize, Debug)]
pub struct FundingRequest {
    client_id: String,
    satoshi: u64,
    no_of_outpoints: u32,
    multiple_tx: bool,
    locking_script: String,
}

#[derive(Deserialize, Debug)]
pub struct ClientAddRequest {
    client_id: String,
    #[serde(default)]
    wif: Option<String>,
    #[serde(default)]
    wif_env: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
}

impl ClientAddRequest {
    fn into_client_config(self) -> Result<ClientConfig, String> {
        let wif_key = match (self.wif, self.wif_env) {
            (Some(wif), None) => wif,
            (None, Some(env_var)) => {
                validate_env_var_name(&env_var)?;
                secret_reference(&env_var)
            }
            (Some(_), Some(_)) => {
                return Err("Provide either wif or wif_env, not both".to_string());
            }
            (None, None) => return Err("Missing wif or wif_env".to_string()),
        };

        let api_key = match (self.api_key, self.api_key_env) {
            (None, None) => None,
            (Some(key), None) => Some(key),
            (None, Some(env_var)) => {
                validate_env_var_name(&env_var)?;
                Some(secret_reference(&env_var))
            }
            (Some(_), Some(_)) => {
                return Err("Provide either api_key or api_key_env, not both".to_string());
            }
        };

        Ok(ClientConfig {
            client_id: self.client_id,
            wif_key,
            api_key,
        })
    }
}

/// Application State Data
pub struct AppState {
    pub service: Arc<Service>,
}

/// Get Index endpoint
#[get("/")]
pub async fn index(_data: web::Data<AppState>) -> String {
    "Financing Service REST API".to_string()
}

/// Health check endpoint for container orchestration
#[get("/health")]
pub async fn health() -> impl Responder {
    json_ok(&HealthResponse::ok())
}

/// Get Service Status endpoint
#[get("/status")]
pub async fn status(data: web::Data<AppState>) -> impl Responder {
    log::info!("status");
    json_ok(&data.service.get_status().await)
}

/// Endpoint to update all the clients, called by ticker every minute
pub async fn update_clients(data: web::Data<AppState>) -> impl Responder {
    Service::refresh_balances(&data.service).await;
    HttpResponse::Ok().finish()
}

/// Post Fund endpoint
/// Example:
///     curl --header "Content-Type: application/json" \
///     --request POST \
///     --data '{"client_id":"id1","satoshi":"123","no_of_outpoints":1,"multiple_tx":false,"locking_script":"00000"}' \
///    http://127.0.0.1:8080/fund

#[post("/fund")]
pub async fn get_funds(
    req: HttpRequest,
    data: web::Data<AppState>,
    info: web::Json<FundingRequest>,
) -> impl Responder {
    log::info!("get_funds");

    let client_id = &info.client_id;
    let satoshi = info.satoshi;
    let no_of_outpoints = info.no_of_outpoints;
    let multiple_tx = info.multiple_tx;
    let locking_script = &info.locking_script;

    if satoshi == 0 {
        return error_response(format!("Invalid satoshi value '{satoshi}'"));
    }
    if no_of_outpoints == 0 {
        return error_response(format!("Invalid no_of_outpoints value '{no_of_outpoints}'"));
    }

    let locking_script_as_bytes = match hex::decode(locking_script) {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(format!(
                "Unable to convert locking_script to bytes '{locking_script}'"
            ));
        }
    };
    debug!("locking_script_as_bytes = {:?}", &locking_script_as_bytes);

    let fund_request = FundRequest {
        client_id: client_id.to_string(),
        satoshi,
        no_of_outpoints,
        multiple_tx,
        locking_script: locking_script_as_bytes,
    };

    if !data.service.is_client_id_valid(client_id).await {
        return error_response(format!("Unknown client_id {client_id}"));
    }
    if let Some(response) = authorize_client(&req, client_id, &data.service).await {
        return response;
    }

    if let Err(description) = Service::refresh_client_chain_state(&data.service, client_id).await {
        log::warn!("refresh_client_chain_state failed: {}", description);
        return error_response(description);
    }

    if fund_request.no_of_outpoints > 1 && fund_request.multiple_tx {
        return match Service::fund_with_multiple_transactions(&data.service, &fund_request).await {
            Ok(funding_response) => match funding_response.to_response() {
                Ok(response) => json_ok(&response),
                Err(description) => error_response(description),
            },
            Err(description) => {
                debug!("fund_with_multiple_transactions error = {:?}", &description);
                error_response(description)
            }
        };
    }

    if let Some(description) = data.service.funding_balance_error(&fund_request).await {
        log::info!("insufficient funds: {}", &description);
        return error_response(description);
    }

    let (blockchain, prepared) =
        match Service::prepare_funding_outpoints(&data.service, &fund_request).await {
            Ok(prepared) => prepared,
            Err(description) => {
                debug!("prepare_funding_outpoints error = {:?}", &description);
                return error_response(description);
            }
        };

    match Service::broadcast_prepared_funding(blockchain, prepared).await {
        Ok(funding_response) => match funding_response.to_response() {
            Ok(response) => json_ok(&response),
            Err(description) => error_response(description),
        },
        Err(description) => {
            debug!("broadcast_prepared_funding error = {:?}", &description);
            error_response(description)
        }
    }
}

/// Add client
/// Example:
///     curl -H "Content-Type: application/json" \
///     --request POST \
///     --data '{"client_id":"client15","wif":"cVL...............qWh"}' \
///   http://127.0.0.1:8082/client
/// {"status": "Success"}
#[post("/client")]
pub async fn add_client(
    req: HttpRequest,
    data: web::Data<AppState>,
    info: web::Json<ClientAddRequest>,
) -> impl Responder {
    if let Some(response) = authorize_admin(&req, &data.service) {
        return response;
    }
    let client_config = match info.into_inner().into_client_config() {
        Ok(client_config) => client_config,
        Err(description) => return error_response(description),
    };
    let client_id = &client_config.client_id;
    log::info!("add_client {}", client_id);

    if data.service.is_client_id_valid(client_id).await {
        return error_response(format!("Client already exists: {client_id}"));
    }

    match data.service.add_client(&client_config).await {
        Ok(()) => json_ok(&SuccessResponse::new()),
        Err(description) => error_response(description),
    }
}

/// Delete client
/// Example:
///     curl -X POST http://127.0.0.1:8080/client/client_1/
#[delete("/client/{client_id}")]
pub async fn delete_client(
    req: HttpRequest,
    data: web::Data<AppState>,
    info: web::Path<String>,
) -> impl Responder {
    let client_id: String = info.to_string();
    log::info!("delete_client {}", &client_id);

    if !data.service.is_client_id_valid(&client_id).await {
        return error_response(format!("Unknown client_id {client_id}"));
    }
    if let Some(response) = authorize_client(&req, &client_id, &data.service).await {
        return response;
    }

    match data.service.delete_client(&client_id).await {
        Ok(()) => json_ok(&SuccessResponse::new()),
        Err(description) => error_response(description),
    }
}

/// Get Address for a particular client_id
#[get("/client/{client_id}/address")]
pub async fn get_address(
    req: HttpRequest,
    data: web::Data<AppState>,
    info: web::Path<String>,
) -> impl Responder {
    let client_id: String = info.to_string();
    log::info!("get address {}", &client_id);

    if !data.service.is_client_id_valid(&client_id).await {
        return error_response(format!("Unknown client_id {client_id}"));
    }
    if let Some(response) = authorize_client(&req, &client_id, &data.service).await {
        return response;
    }

    match data.service.get_address(&client_id).await {
        Some(address) => json_ok(&AddressResponse { address }),
        None => error_response(format!("Unknown client_id {client_id}")),
    }
}

/// Get Balance for a particular client_id endpoint
#[get("/client/{client_id}/balance")]
pub async fn balance(
    req: HttpRequest,
    data: web::Data<AppState>,
    info: web::Path<String>,
) -> impl Responder {
    let client_id: String = info.to_string();
    log::info!("get balance {}", &client_id);

    if !data.service.is_client_id_valid(&client_id).await {
        return error_response(format!("Unknown client_id {client_id}"));
    }
    if let Some(response) = authorize_client(&req, &client_id, &data.service).await {
        return response;
    }

    if let Err(description) = Service::refresh_client_chain_state(&data.service, &client_id).await {
        log::warn!("refresh_client_chain_state failed: {}", description);
        return error_response(description);
    }

    match data.service.get_balance(&client_id).await {
        Some(balance) => json_ok(&BalanceResponse {
            confirmed: balance.confirmed,
            unconfirmed: balance.unconfirmed,
        }),
        None => error_response(format!("Unknown client_id {client_id}")),
    }
}

#[cfg(test)]
mod tests {
    use actix_governor::Governor;
    use actix_http::{body::BoxBody, Request};
    use actix_web::body::EitherBody;
    use actix_web::{dev::Service as ActixService, dev::ServiceResponse, Error};
    use actix_web::{http::StatusCode, test, web, App};
    use serde_json::{json, Value};
    use std::sync::Arc;

    use crate::{
        config::RateLimitConfig,
        rate_limit,
        rest_api::{
            add_client, balance, delete_client, get_address, get_funds, health, index, status,
            AppState,
        },
        service::Service,
        test_support::{
            test_blockchain_interface, test_config_with_keys, unique_dynamic_config_path,
            LOCKING_SCRIPT_HEX, TEST_ADDRESS, TEST_CLIENT_ID, TEST_WIF,
        },
    };

    const TEST_API_KEY: &str = "test-secret-key";
    const TEST_ADMIN_API_KEY: &str = "test-admin-key";

    async fn build_app_with_rate_limit(
        burst_size: u32,
    ) -> impl ActixService<Request, Response = ServiceResponse<EitherBody<BoxBody>>, Error = Error>
    {
        let mut config = test_config_with_keys(&unique_dynamic_config_path(), None, None);
        config.web_interface.rate_limit = RateLimitConfig {
            enabled: true,
            requests_per_second: 1,
            burst_size: Some(burst_size),
        };
        let governor_config = rate_limit::build_governor_config(&config.web_interface.rate_limit);
        let blockchain = test_blockchain_interface(&config).await;
        let financing_service = Arc::new(Service::new_for_test(&config, blockchain).await);
        let app_state = web::Data::new(AppState {
            service: financing_service,
        });

        test::init_service(
            App::new()
                .wrap(Governor::new(&governor_config))
                .app_data(app_state)
                .service(index)
                .service(health)
                .service(status)
                .service(balance)
                .service(get_funds)
                .service(add_client)
                .service(delete_client)
                .service(get_address),
        )
        .await
    }

    fn peer_addr() -> std::net::SocketAddr {
        "127.0.0.1:8080".parse().expect("valid peer address")
    }

    async fn build_app_with_keys(
        client_api_key: Option<&str>,
        admin_api_key: Option<&str>,
    ) -> impl ActixService<Request, Response = ServiceResponse, Error = Error> {
        let config =
            test_config_with_keys(&unique_dynamic_config_path(), client_api_key, admin_api_key);
        let blockchain = test_blockchain_interface(&config).await;
        let financing_service = Arc::new(Service::new_for_test(&config, blockchain).await);
        let app_state = web::Data::new(AppState {
            service: financing_service,
        });

        test::init_service(
            App::new()
                .app_data(app_state)
                .service(index)
                .service(health)
                .service(status)
                .service(balance)
                .service(get_funds)
                .service(add_client)
                .service(delete_client)
                .service(get_address),
        )
        .await
    }

    async fn build_app_with_api_key(
        api_key: Option<&str>,
    ) -> impl ActixService<Request, Response = ServiceResponse, Error = Error> {
        build_app_with_keys(api_key, None).await
    }

    async fn build_app_with_admin_key(
        admin_api_key: Option<&str>,
    ) -> impl ActixService<Request, Response = ServiceResponse, Error = Error> {
        build_app_with_keys(None, admin_api_key).await
    }

    async fn build_app() -> impl ActixService<Request, Response = ServiceResponse, Error = Error> {
        build_app_with_keys(None, None).await
    }

    fn fund_body(
        client_id: &str,
        satoshi: u64,
        no_of_outpoints: u32,
        locking_script: &str,
    ) -> Value {
        json!({
            "client_id": client_id,
            "satoshi": satoshi,
            "no_of_outpoints": no_of_outpoints,
            "multiple_tx": false,
            "locking_script": locking_script,
        })
    }

    #[actix_web::test]
    async fn test_index() {
        let app = build_app().await;
        let resp = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = test::read_body(resp).await;
        assert_eq!(body, "Financing Service REST API");
    }

    #[actix_web::test]
    async fn test_health() {
        let app = build_app().await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/health").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "ok");
    }

    #[actix_web::test]
    async fn test_status() {
        let app = build_app().await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/status").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["blockchain_status"].is_string());
    }

    #[actix_web::test]
    async fn test_balance_unknown_client() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/client/unknown/balance")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("Unknown client_id"));
    }

    #[actix_web::test]
    async fn test_balance_success() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!("/client/{TEST_CLIENT_ID}/balance"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["confirmed"].as_i64().unwrap() > 0);
    }

    #[actix_web::test]
    async fn test_address_success() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!("/client/{TEST_CLIENT_ID}/address"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["address"], TEST_ADDRESS);
    }

    #[actix_web::test]
    async fn test_address_unknown_client() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/client/unknown/address")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[actix_web::test]
    async fn test_add_client_with_wif_env() {
        unsafe { std::env::set_var("FS_TEST_ADD_CLIENT_WIF", TEST_WIF) };
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .set_json(json!({
                    "client_id": "client_env",
                    "wif_env": "FS_TEST_ADD_CLIENT_WIF",
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "Success");
    }

    #[actix_web::test]
    async fn test_add_client_success() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .set_json(json!({
                    "client_id": "client2",
                    "wif": TEST_WIF,
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "Success");
    }

    #[actix_web::test]
    async fn test_add_client_duplicate() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .set_json(json!({
                    "client_id": TEST_CLIENT_ID,
                    "wif": TEST_WIF,
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("already exists"));
    }

    #[actix_web::test]
    async fn test_add_client_invalid_wif() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .set_json(json!({
                    "client_id": "client3",
                    "wif": "not-a-valid-wif",
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("not a valid WIF key"));
    }

    #[actix_web::test]
    async fn test_delete_client_success() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/client/{TEST_CLIENT_ID}"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["status"], "Success");
    }

    #[actix_web::test]
    async fn test_delete_client_unknown() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri("/client/unknown")
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[actix_web::test]
    async fn test_fund_unknown_client() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body("unknown", 123, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("Unknown client_id"));
    }

    #[actix_web::test]
    async fn test_fund_invalid_satoshi() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(TEST_CLIENT_ID, 0, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("Invalid satoshi"));
    }

    #[actix_web::test]
    async fn test_fund_invalid_no_of_outpoints() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(TEST_CLIENT_ID, 123, 0, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("Invalid no_of_outpoints"));
    }

    #[actix_web::test]
    async fn test_fund_invalid_locking_script() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(TEST_CLIENT_ID, 123, 1, "not-hex"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("locking_script"));
    }

    #[actix_web::test]
    async fn test_fund_insufficient_balance() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(
                    TEST_CLIENT_ID,
                    100_000_000,
                    1,
                    LOCKING_SCRIPT_HEX,
                ))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("Insufficent client balance"));
    }

    #[actix_web::test]
    async fn test_fund_consolidates_multiple_utxos() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(TEST_CLIENT_ID, 50_000_000, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["txs"].as_array().unwrap().len(), 1);
    }

    #[actix_web::test]
    async fn test_fund_multiple_tx_success() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(json!({
                    "client_id": TEST_CLIENT_ID,
                    "satoshi": 123,
                    "no_of_outpoints": 2,
                    "multiple_tx": true,
                    "locking_script": LOCKING_SCRIPT_HEX,
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["txs"].as_array().unwrap().len(), 2);
        assert_eq!(body["outpoints"].as_array().unwrap().len(), 2);
    }

    #[actix_web::test]
    async fn test_fund_success() {
        let app = build_app().await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(TEST_CLIENT_ID, 123, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["outpoints"].as_array().unwrap().len(), 1);
        assert_eq!(body["txs"].as_array().unwrap().len(), 1);
        assert!(body["outpoints"][0]["hash"].as_str().unwrap().len() > 0);
    }

    #[actix_web::test]
    async fn test_health_unauthenticated_when_api_key_enabled() {
        let app = build_app_with_api_key(Some(TEST_API_KEY)).await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/health").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn test_fund_requires_api_key_when_enabled() {
        let app = build_app_with_api_key(Some(TEST_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .set_json(fund_body(TEST_CLIENT_ID, 123, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: Value = test::read_body_json(resp).await;
        assert_eq!(body["description"], "Unauthorized");
    }

    #[actix_web::test]
    async fn test_fund_accepts_bearer_token() {
        let app = build_app_with_api_key(Some(TEST_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .insert_header(("Authorization", format!("Bearer {TEST_API_KEY}")))
                .set_json(fund_body(TEST_CLIENT_ID, 123, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn test_add_client_requires_admin_key_when_enabled() {
        let app = build_app_with_admin_key(Some(TEST_ADMIN_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .set_json(json!({
                    "client_id": "client2",
                    "wif": TEST_WIF,
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn test_add_client_accepts_admin_bearer_token() {
        let app = build_app_with_admin_key(Some(TEST_ADMIN_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .insert_header(("Authorization", format!("Bearer {TEST_ADMIN_API_KEY}")))
                .set_json(json!({
                    "client_id": "client2",
                    "wif": TEST_WIF,
                    "api_key": "new-client-key",
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn test_add_client_rejects_wrong_admin_key() {
        let app = build_app_with_admin_key(Some(TEST_ADMIN_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/client")
                .insert_header(("X-API-Key", "wrong-admin-key"))
                .set_json(json!({
                    "client_id": "client2",
                    "wif": TEST_WIF,
                }))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn test_status_unauthenticated_when_client_api_key_enabled() {
        let app = build_app_with_api_key(Some(TEST_API_KEY)).await;
        let resp =
            test::call_service(&app, test::TestRequest::get().uri("/status").to_request()).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[actix_web::test]
    async fn test_balance_requires_client_api_key_when_enabled() {
        let app = build_app_with_api_key(Some(TEST_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!("/client/{TEST_CLIENT_ID}/balance"))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn test_fund_rejects_wrong_client_api_key() {
        let app = build_app_with_api_key(Some(TEST_API_KEY)).await;
        let resp = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/fund")
                .insert_header(("X-API-Key", "wrong-key"))
                .set_json(fund_body(TEST_CLIENT_ID, 123, 1, LOCKING_SCRIPT_HEX))
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn test_rate_limit_returns_429_when_burst_exceeded() {
        let app = build_app_with_rate_limit(2).await;
        for _ in 0..2 {
            let resp = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/")
                    .peer_addr(peer_addr())
                    .to_request(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }

        let resp = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/")
                .peer_addr(peer_addr())
                .to_request(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        let body: Value = test::read_body_json(resp).await;
        assert!(body["description"]
            .as_str()
            .unwrap()
            .contains("Rate limit exceeded"));
    }

    #[actix_web::test]
    async fn test_health_is_exempt_from_rate_limit() {
        let app = build_app_with_rate_limit(1).await;
        for _ in 0..5 {
            let resp = test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/health")
                    .peer_addr(peer_addr())
                    .to_request(),
            )
            .await;
            assert_eq!(resp.status(), StatusCode::OK);
        }
    }
}
