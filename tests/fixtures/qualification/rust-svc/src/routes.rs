use axum::routing::get;

pub fn router() {
    let _app = axum::Router::new().route("/orders", get(list_orders));
}
