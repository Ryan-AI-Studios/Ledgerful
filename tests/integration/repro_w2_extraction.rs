#[cfg(test)]
mod tests {
    use ledgerful::index::languages::rust::routes::extract_routes;

    #[test]
    fn test_axum_auth_and_schema_extraction() {
        let content = r#"
            use axum::{routing::get, Router, Json};
            use serde::Deserialize;

            #[derive(Deserialize)]
            struct CreateUser { name: String }

            async fn create_user(Json(payload): Json<CreateUser>) {}

            async fn get_users() {}

            pub fn app() -> Router {
                Router::new()
                    .route("/users", get(get_users))
                    .route("/users", axum::routing::post(create_user))
                    .layer(axum::middleware::from_fn(auth))
            }

            async fn auth(req: Request, next: Next) -> Response { next.run(req).await }
        "#;

        let routes = extract_routes(content, &[]).unwrap();

        // Find POST /users
        let post_route = routes
            .iter()
            .find(|r| r.method == "POST" && r.path_pattern == "/users")
            .expect("POST /users not found");

        assert_eq!(post_route.handler_name, "create_user");
        // These will fail currently
        assert!(
            post_route
                .schema_refs
                .as_ref()
                .map(|s| s.contains(&"CreateUser".to_string()))
                .unwrap_or(false),
            "Schema CreateUser not found in {:?}",
            post_route.schema_refs
        );
        assert!(
            post_route
                .auth_requirements
                .as_ref()
                .map(|a| a.contains(&"secured".to_string()))
                .unwrap_or(false),
            "Auth requirement 'secured' not found in {:?}",
            post_route.auth_requirements
        );
    }
}
