use actix_web::{delete, get, http::header::ContentType, post, web, HttpResponse, Responder};
use async_mutex::Mutex;
use log::debug;
use serde::Deserialize;

use crate::{
    client::FundRequest,
    error::error_response,
    service::Service,
};

/// Application State Data
pub struct AppState {
    pub service: Mutex<Service>,
}

/// Get Index endpoint
#[get("/")]
pub async fn index(_data: web::Data<AppState>) -> String {
    "Financing Service REST API".to_string()
}

/// Get Service Status endpoint
#[get("/status")]
pub async fn status(data: web::Data<AppState>) -> impl Responder {
    log::info!("status");

    let service = data.service.lock().await;
    let status = service.get_status();
    HttpResponse::Ok()
        .content_type(ContentType::json())
        .body(status)
}

/// Endpoint to update all the clients, called by ticker every minute
pub async fn update_clients(data: web::Data<AppState>) -> impl Responder {
    let mut service = data.service.lock().await;
    service.update_balances().await;
    HttpResponse::Ok()
}

/// This is the /fund API call request
#[derive(Deserialize, Debug)]
pub struct FundingRequest {
    client_id: String,
    satoshi: u64,
    no_of_outpoints: u32,
    multiple_tx: bool,
    locking_script: String,
}

/// Post Fund endpoint
/// Example:
///     curl --header "Content-Type: application/json" \
///     --request POST \
///     --data '{"client_id":"id1","satoshi":"123","no_of_outpoints":1,"multiple_tx":false,"locking_script":"00000"}' \
///    http://127.0.0.1:8080/fund

#[post("/fund")]
pub async fn get_funds(
    data: web::Data<AppState>,
    info: web::Json<FundingRequest>,
) -> impl Responder {
    log::info!("get_funds");

    let mut service = data.service.lock().await;

    let client_id = &info.client_id;
    let satoshi = info.satoshi;
    let no_of_outpoints = info.no_of_outpoints;
    let multiple_tx = info.multiple_tx;
    let locking_script = &info.locking_script;

    if !service.is_client_id_valid(client_id) {
        return error_response(format!("Unknown client_id {client_id}"));
    }
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

    match service.has_sufficent_balance(&fund_request) {
        Some(true) => {}
        Some(false) | None => {
            log::info!("insufficient funds!");
            return error_response(
                "Insufficent client balance to create funding transactions.",
            );
        }
    }

    match service.create_funding_outpoints(&fund_request).await {
        Ok(funding_response) => match funding_response.to_json() {
            Ok(body) => HttpResponse::Ok()
                .content_type(ContentType::json())
                .body(body),
            Err(description) => error_response(description),
        },
        Err(description) => {
            debug!("create_funding_outpoints error = {:?}", &description);
            error_response(description)
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct ClienAddRequest {
    client_id: String,
    wif: String,
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
    data: web::Data<AppState>,
    info: web::Json<ClienAddRequest>,
) -> impl Responder {
    let mut service = data.service.lock().await;
    let client_id = &info.client_id;
    log::info!("add_client {}", &client_id);

    if service.is_client_id_valid(client_id) {
        return error_response(format!("Client already exists: {client_id}"));
    }

    match service.add_client(client_id, &info.wif) {
        Ok(()) => HttpResponse::Ok()
            .content_type(ContentType::json())
            .body("{\"status\": \"Success\"}"),
        Err(description) => error_response(description),
    }
}

/// Delete client
/// Example:
///     curl -X POST http://127.0.0.1:8080/client/client_1/
#[delete("/client/{client_id}")]
pub async fn delete_client(data: web::Data<AppState>, info: web::Path<String>) -> impl Responder {
    let mut service = data.service.lock().await;
    let client_id: String = info.to_string();
    log::info!("delete_client {}", &client_id);

    if !service.is_client_id_valid(&client_id) {
        return error_response(format!("Unknown client_id {client_id}"));
    }

    match service.delete_client(&client_id) {
        Ok(()) => HttpResponse::Ok()
            .content_type(ContentType::json())
            .body("{\"status\": \"Success\"}"),
        Err(description) => error_response(description),
    }
}

/// Get Address for a particular client_id
#[get("/client/{client_id}/address")]
pub async fn get_address(data: web::Data<AppState>, info: web::Path<String>) -> impl Responder {
    let client_id: String = info.to_string();
    log::info!("get address {}", &client_id);

    let service = data.service.lock().await;

    if !service.is_client_id_valid(&client_id) {
        return error_response(format!("Unknown client_id {client_id}"));
    }

    match service.get_address(&client_id) {
        Some(address) => HttpResponse::Ok()
            .content_type(ContentType::json())
            .body(format!("{{\"address\": \"{address}\"}}")),
        None => error_response(format!("Unknown client_id {client_id}")),
    }
}

/// Get Balance for a particular client_id endpoint
#[get("/client/{client_id}/balance")]
pub async fn balance(data: web::Data<AppState>, info: web::Path<String>) -> impl Responder {
    let client_id: String = info.to_string();
    log::info!("get balance {}", &client_id);

    let service = data.service.lock().await;

    if !service.is_client_id_valid(&client_id) {
        return error_response(format!("Unknown client_id {client_id}"));
    }

    match service.get_balance(&client_id) {
        Some(balance) => {
            let confirmed = balance.confirmed;
            let unconfirmed = balance.unconfirmed;
            HttpResponse::Ok()
                .content_type(ContentType::json())
                .body(format!(
                    "{{\"confirmed\": {confirmed}, \"unconfirmed\": {unconfirmed}}}"
                ))
        }
        None => error_response(format!("Unknown client_id {client_id}")),
    }
}
