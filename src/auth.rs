use std::{
    future::{ready, Ready},
    sync::Arc,
};

use actix_web::{
    body::EitherBody,
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    http::header,
    Error,
};
use futures_util::future::LocalBoxFuture;

use crate::responses::unauthorized_response;

const BEARER_PREFIX: &str = "Bearer ";
const API_KEY_HEADER: &str = "X-API-Key";

#[derive(Clone)]
pub struct ApiKeyAuth {
    api_key: Option<Arc<String>>,
}

impl ApiKeyAuth {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key: api_key.filter(|key| !key.is_empty()).map(Arc::new),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.api_key.is_some()
    }
}

fn extract_api_key(req: &ServiceRequest, expected: &str) -> bool {
    if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix(BEARER_PREFIX) {
                return constant_time_eq(token, expected);
            }
        }
    }

    if let Some(key_header) = req.headers().get(API_KEY_HEADER) {
        if let Ok(key_str) = key_header.to_str() {
            return constant_time_eq(key_str, expected);
        }
    }

    false
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

impl<S, B> Transform<S, ServiceRequest> for ApiKeyAuth
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = ApiKeyAuthMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(ApiKeyAuthMiddleware {
            service,
            api_key: self.api_key.clone(),
        }))
    }
}

pub struct ApiKeyAuthMiddleware<S> {
    service: S,
    api_key: Option<Arc<String>>,
}

impl<S, B> Service<ServiceRequest> for ApiKeyAuthMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        if let Some(expected) = self.api_key.as_ref() {
            if !extract_api_key(&req, expected) {
                let response = unauthorized_response();
                return Box::pin(
                    async move { Ok(req.into_response(response.map_into_right_body())) },
                );
            }
        }

        let fut = self.service.call(req);
        Box::pin(async move {
            let res = fut.await?;
            Ok(res.map_into_left_body())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, ApiKeyAuth, BEARER_PREFIX};
    use actix_web::{
        test::{self, TestRequest},
        web, App, HttpResponse,
    };

    async fn ok_handler() -> HttpResponse {
        HttpResponse::Ok().finish()
    }

    #[test]
    fn constant_time_eq_matches_equal_strings() {
        assert!(constant_time_eq("secret", "secret"));
    }

    #[test]
    fn constant_time_eq_rejects_different_strings() {
        assert!(!constant_time_eq("secret", "other"));
        assert!(!constant_time_eq("secret", "secre"));
    }

    #[actix_web::test]
    async fn middleware_allows_request_when_auth_disabled() {
        let app = test::init_service(
            App::new()
                .wrap(ApiKeyAuth::new(None))
                .route("/", web::get().to(ok_handler)),
        )
        .await;

        let resp = test::call_service(&app, TestRequest::get().uri("/").to_request()).await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn middleware_rejects_missing_api_key() {
        let app = test::init_service(
            App::new()
                .wrap(ApiKeyAuth::new(Some("secret".to_string())))
                .route("/", web::get().to(ok_handler)),
        )
        .await;

        let resp = test::call_service(&app, TestRequest::get().uri("/").to_request()).await;
        assert_eq!(resp.status(), 401);
    }

    #[actix_web::test]
    async fn middleware_accepts_bearer_token() {
        let app = test::init_service(
            App::new()
                .wrap(ApiKeyAuth::new(Some("secret".to_string())))
                .route("/", web::get().to(ok_handler)),
        )
        .await;

        let resp = test::call_service(
            &app,
            TestRequest::get()
                .uri("/")
                .insert_header(("Authorization", format!("{BEARER_PREFIX}secret")))
                .to_request(),
        )
        .await;
        assert!(resp.status().is_success());
    }

    #[actix_web::test]
    async fn middleware_accepts_x_api_key_header() {
        let app = test::init_service(
            App::new()
                .wrap(ApiKeyAuth::new(Some("secret".to_string())))
                .route("/", web::get().to(ok_handler)),
        )
        .await;

        let resp = test::call_service(
            &app,
            TestRequest::get()
                .uri("/")
                .insert_header(("X-API-Key", "secret"))
                .to_request(),
        )
        .await;
        assert!(resp.status().is_success());
    }
}
