//! OpenAPI coverage tests for the generated public REST schema.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::{Value, json};
use thewiki_api::app;
use thewiki_storage::sqlite::SqliteStorage;

fn schema() -> Value {
    serde_json::to_value(app::openapi::<SqliteStorage>()).expect("serialize openapi")
}

fn parameter_location(doc: &Value, path: &str, method: &str, name: &str) -> Option<String> {
    doc["paths"][path][method]["parameters"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == name)
        .and_then(|p| p["in"].as_str())
        .map(str::to_string)
}

fn parameter_required(doc: &Value, path: &str, method: &str, name: &str) -> Option<bool> {
    doc["paths"][path][method]["parameters"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == name)
        .and_then(|p| p["required"].as_bool())
}

fn parameter_description(doc: &Value, path: &str, method: &str, name: &str) -> Option<String> {
    doc["paths"][path][method]["parameters"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == name)
        .and_then(|p| p["description"].as_str())
        .map(str::to_string)
}

fn response_schema_ref(doc: &Value, path: &str, method: &str, status: &str) -> Option<String> {
    doc["paths"][path][method]["responses"][status]["content"]["application/json"]["schema"]["$ref"]
        .as_str()
        .map(str::to_string)
}

fn response_description(doc: &Value, path: &str, method: &str, status: &str) -> Option<String> {
    doc["paths"][path][method]["responses"][status]["description"]
        .as_str()
        .map(str::to_string)
}

fn operation_security<'a>(doc: &'a Value, path: &str, method: &str) -> Option<&'a [Value]> {
    doc["paths"][path][method]["security"]
        .as_array()
        .map(Vec::as_slice)
}

#[test]
fn public_auth_endpoints_are_documented() {
    let doc = schema();
    let paths = doc["paths"].as_object().expect("paths object");

    assert!(paths["/api/v1/auth/login"].get("post").is_some());
    assert!(paths["/api/v1/auth/logout"].get("post").is_some());
    assert!(paths["/api/v1/auth/me"].get("get").is_some());
    assert!(paths["/api/v1/auth/policy"].get("get").is_some());
}

#[test]
fn committed_openapi_snapshot_matches_generated_schema() {
    let generated = schema();
    let committed: Value = serde_json::from_str(include_str!("../../../docs/openapi.json"))
        .expect("parse committed openapi.json");

    assert_eq!(
        generated, committed,
        "docs/openapi.json is stale; regenerate via \
         `cargo run --locked -p thewiki-api -- openapi > docs/openapi.json`"
    );
}

#[test]
fn query_parameters_are_not_emitted_as_path_parameters() {
    let doc = schema();

    assert_eq!(
        parameter_location(&doc, "/api/v1/pages", "get", "limit").as_deref(),
        Some("query")
    );
    assert_eq!(
        parameter_location(&doc, "/api/v1/pages/{slug}/revisions", "get", "cursor").as_deref(),
        Some("query")
    );
    assert_eq!(
        parameter_location(&doc, "/api/v1/pages/{slug}/diff", "get", "from").as_deref(),
        Some("query")
    );
    assert_eq!(
        parameter_location(&doc, "/api/v1/recent-changes", "get", "since").as_deref(),
        Some("query")
    );
}

#[test]
fn auth_security_schemes_are_documented() {
    let doc = schema();
    let schemes = &doc["components"]["securitySchemes"];

    assert_eq!(schemes["SessionCookie"]["type"], "apiKey");
    assert_eq!(schemes["SessionCookie"]["in"], "cookie");
    assert_eq!(schemes["SessionCookie"]["name"], "thewiki_session");
    assert_eq!(schemes["CsrfToken"]["type"], "apiKey");
    assert_eq!(schemes["CsrfToken"]["in"], "header");
    assert_eq!(schemes["CsrfToken"]["name"], "x-csrf-token");
}

#[test]
fn auth_operations_document_security_requirements() {
    let doc = schema();
    let optional_session = [json!({}), json!({ "CsrfToken": [], "SessionCookie": [] })];

    assert_eq!(
        operation_security(&doc, "/api/v1/auth/login", "post").expect("login security"),
        optional_session.as_slice(),
        "login accepts credentials without auth, but CSRF applies when a session cookie is present"
    );
    assert_eq!(
        operation_security(&doc, "/api/v1/auth/me", "get").expect("/me security"),
        [json!({ "SessionCookie": [] })].as_slice()
    );
    assert_eq!(
        operation_security(&doc, "/api/v1/auth/logout", "post").expect("logout security"),
        [json!({ "CsrfToken": [], "SessionCookie": [] })].as_slice()
    );
}

#[test]
fn login_documents_conditional_csrf_behavior() {
    let doc = schema();

    let cookie_description = parameter_description(&doc, "/api/v1/auth/login", "post", "cookie")
        .expect("login cookie parameter description");
    let forbidden_description = response_description(&doc, "/api/v1/auth/login", "post", "403")
        .expect("login 403 response description");

    assert!(cookie_description.contains("when a session cookie is present"));
    assert!(forbidden_description.contains("when a session cookie is present"));
    assert_eq!(
        parameter_required(&doc, "/api/v1/auth/login", "post", "cookie"),
        Some(false)
    );
    assert_eq!(
        parameter_required(&doc, "/api/v1/auth/login", "post", "x-csrf-token"),
        Some(false)
    );
    assert_eq!(
        response_schema_ref(&doc, "/api/v1/auth/login", "post", "403").as_deref(),
        Some("#/components/schemas/AuthErrorBody")
    );
}

#[test]
fn api_operations_document_rate_limit_response() {
    let doc = schema();
    let paths = doc["paths"].as_object().expect("paths object");
    let methods = ["get", "post", "put", "delete", "patch", "head", "options"];

    assert_eq!(
        doc["components"]["schemas"]["RateLimitErrorBody"]["properties"]["error"]["type"],
        "string"
    );

    for (path, item) in paths
        .iter()
        .filter(|(path, _)| path.starts_with("/api/v1/"))
    {
        for method in methods {
            if item.get(method).is_some() {
                assert_eq!(
                    response_schema_ref(&doc, path, method, "429").as_deref(),
                    Some("#/components/schemas/RateLimitErrorBody"),
                    "{method} {path} should document the rate-limit error body"
                );
            }
        }
    }
}

#[test]
fn logout_documents_both_session_and_csrf_cookie_inputs() {
    let doc = schema();
    let cookie_description = doc["paths"]["/api/v1/auth/logout"]["post"]["parameters"]
        .as_array()
        .expect("logout parameters")
        .iter()
        .find(|p| p["name"] == "cookie")
        .and_then(|p| p["description"].as_str())
        .expect("logout cookie parameter description");

    assert!(cookie_description.contains("thewiki_session"));
    assert!(cookie_description.contains("thewiki_csrf"));
}

#[test]
fn mutating_page_endpoints_document_csrf_inputs_and_error_body() {
    let doc = schema();
    for (path, method) in [
        ("/api/v1/pages", "post"),
        ("/api/v1/pages/{slug}", "put"),
        ("/api/v1/pages/{slug}", "delete"),
        ("/api/v1/pages/{slug}/revert", "post"),
    ] {
        assert_eq!(
            parameter_location(&doc, path, method, "cookie").as_deref(),
            Some("header"),
            "{method} {path} should document the session/csrf cookie header"
        );
        assert_eq!(
            parameter_location(&doc, path, method, "x-csrf-token").as_deref(),
            Some("header"),
            "{method} {path} should document the csrf token header"
        );
        assert_eq!(
            response_schema_ref(&doc, path, method, "403").as_deref(),
            Some("#/components/schemas/AuthErrorBody"),
            "{method} {path} should document the runtime CSRF error body"
        );
    }
}

#[test]
fn page_auth_failures_document_runtime_error_body() {
    let doc = schema();
    for (path, method) in [
        ("/api/v1/pages", "post"),
        ("/api/v1/pages/{slug}", "put"),
        ("/api/v1/pages/{slug}", "delete"),
        ("/api/v1/pages/{slug}/revert", "post"),
    ] {
        assert_eq!(
            response_schema_ref(&doc, path, method, "401").as_deref(),
            Some("#/components/schemas/ErrorBody"),
            "{method} {path} should document the runtime page auth error body"
        );
    }
}

#[test]
fn page_mutators_document_optional_authenticated_security() {
    let doc = schema();
    let expected = [json!({}), json!({ "CsrfToken": [], "SessionCookie": [] })];

    for (path, method) in [
        ("/api/v1/pages", "post"),
        ("/api/v1/pages/{slug}", "put"),
        ("/api/v1/pages/{slug}", "delete"),
    ] {
        assert_eq!(
            operation_security(&doc, path, method).expect("page mutator security"),
            expected.as_slice(),
            "{method} {path} should allow anonymous edit mode or authenticated+csrf edits"
        );
    }
}

#[test]
fn anonymously_editable_page_mutators_do_not_require_auth_headers() {
    let doc = schema();
    for (path, method) in [
        ("/api/v1/pages", "post"),
        ("/api/v1/pages/{slug}", "put"),
        ("/api/v1/pages/{slug}", "delete"),
    ] {
        assert_eq!(
            parameter_required(&doc, path, method, "cookie"),
            Some(false),
            "{method} {path} can be anonymous when configured"
        );
        assert_eq!(
            parameter_required(&doc, path, method, "x-csrf-token"),
            Some(false),
            "{method} {path} only needs CSRF when a session cookie is present"
        );
    }
}

#[test]
fn revert_requires_auth_and_csrf_headers() {
    let doc = schema();

    assert_eq!(
        parameter_required(&doc, "/api/v1/pages/{slug}/revert", "post", "cookie"),
        Some(true)
    );
    assert_eq!(
        parameter_required(&doc, "/api/v1/pages/{slug}/revert", "post", "x-csrf-token"),
        Some(true)
    );
    assert_eq!(
        operation_security(&doc, "/api/v1/pages/{slug}/revert", "post").expect("revert security"),
        [json!({ "CsrfToken": [], "SessionCookie": [] })].as_slice()
    );
}
