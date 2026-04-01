use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json, Redirect},
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tower_http::services::ServeDir;

struct AppState {
    db: Mutex<Connection>,
    stripe_key: Option<String>,
    admin_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct OrderItem {
    name: String,
    price: u32,
    qty: u32,
}

#[derive(Deserialize)]
struct CreateOrder {
    name: String,
    phone: String,
    pickup_time: String,
    items: Vec<OrderItem>,
    note: String,
}

#[derive(Serialize)]
struct Order {
    id: String,
    name: String,
    phone: String,
    pickup_time: String,
    items: Vec<OrderItem>,
    note: String,
    total: u32,
    status: String,
    paid: bool,
    created_at: String,
}

#[derive(Deserialize)]
struct UpdateStatus {
    status: String,
}

#[derive(Deserialize)]
struct SuccessQuery {
    order_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MenuItem {
    id: String,
    available: bool,
}

#[derive(Deserialize)]
struct StoreStatus {
    open: Option<bool>,
    notice: Option<String>,
}

fn init_db(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS orders (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            phone TEXT NOT NULL,
            pickup_time TEXT NOT NULL,
            items TEXT NOT NULL,
            note TEXT DEFAULT '',
            total INTEGER NOT NULL,
            status TEXT DEFAULT 'new',
            paid INTEGER DEFAULT 0,
            stripe_session TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS menu_status (
            item_id TEXT PRIMARY KEY,
            available INTEGER DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS store_config (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )
    .expect("Failed to create tables");
    // Migrations
    conn.execute_batch("ALTER TABLE orders ADD COLUMN paid INTEGER DEFAULT 0").ok();
    conn.execute_batch("ALTER TABLE orders ADD COLUMN stripe_session TEXT DEFAULT ''").ok();
    // Default store config
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('open', '1')", []).ok();
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('notice', '')", []).ok();
}

fn check_admin(state: &AppState, headers: &HeaderMap) -> bool {
    if let Some(auth) = headers.get("x-admin-key") {
        return auth.to_str().unwrap_or("") == state.admin_key;
    }
    false
}

#[tokio::main]
async fn main() {
    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "makimaki.db".to_string());
    let conn = Connection::open(&db_path).expect("Failed to open DB");
    conn.execute_batch("PRAGMA journal_mode=WAL;").ok();
    init_db(&conn);

    let stripe_key = std::env::var("STRIPE_SECRET_KEY").ok();
    let admin_key = std::env::var("ADMIN_KEY").unwrap_or_else(|_| "makimaki2026".to_string());

    if stripe_key.is_some() { println!("Stripe payments enabled"); }
    println!("Admin key configured");

    let state = Arc::new(AppState { db: Mutex::new(conn), stripe_key, admin_key });

    let app = Router::new()
        .route("/api/orders", post(create_order).get(list_orders))
        .route("/api/orders/{id}/status", post(update_order_status))
        .route("/api/checkout", post(create_checkout))
        .route("/api/stripe-success", get(stripe_success))
        .route("/api/menu-status", get(get_menu_status).post(update_menu_status))
        .route("/api/store-config", get(get_store_config).post(update_store_config))
        .route("/api/stats", get(get_stats))
        .route("/api/auth", post(check_auth))
        .route("/admin", get(admin_page))
        .fallback_service(ServeDir::new("static"))
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{port}");
    println!("makimaki listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn check_auth(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let key = body["key"].as_str().unwrap_or("");
    if key == state.admin_key {
        (StatusCode::OK, Json(serde_json::json!({"ok": true})))
    } else {
        (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"ok": false})))
    }
}

async fn create_order(
    State(state): State<Arc<AppState>>,
    Json(input): Json<CreateOrder>,
) -> impl IntoResponse {
    // Check if store is open
    {
        let db = state.db.lock().unwrap();
        let open: String = db.query_row("SELECT value FROM store_config WHERE key='open'", [], |r| r.get(0)).unwrap_or("1".into());
        if open != "1" {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "Store is currently closed"})));
        }
    }

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let total: u32 = input.items.iter().map(|i| i.price * i.qty).sum();
    let items_json = serde_json::to_string(&input.items).unwrap_or_default();
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M")
        .to_string();

    let db = state.db.lock().unwrap();
    match db.execute(
        "INSERT INTO orders (id,name,phone,pickup_time,items,note,total,status,paid,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'new',0,?8)",
        rusqlite::params![id, input.name, input.phone, input.pickup_time, items_json, input.note, total, now],
    ) {
        Ok(_) => (StatusCode::CREATED, Json(serde_json::json!({"id": id, "total": total}))),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

async fn create_checkout(
    State(state): State<Arc<AppState>>,
    Json(input): Json<CreateOrder>,
) -> impl IntoResponse {
    let stripe_key = match &state.stripe_key {
        Some(k) => k.clone(),
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "Stripe not configured. Please use pay-at-shop."}))),
    };

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let total: u32 = input.items.iter().map(|i| i.price * i.qty).sum();
    let total_with_tax = ((total as f64) * 1.1).ceil() as u32 + 200;
    let items_json = serde_json::to_string(&input.items).unwrap_or_default();
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M")
        .to_string();

    {
        let db = state.db.lock().unwrap();
        let _ = db.execute(
            "INSERT INTO orders (id,name,phone,pickup_time,items,note,total,status,paid,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'new',0,?8)",
            rusqlite::params![id, input.name, input.phone, input.pickup_time, items_json, input.note, total_with_tax, now],
        );
    }

    let line_items: Vec<Vec<(String, String)>> = input.items.iter().map(|item| {
        let unit_amount = ((item.price as f64) * 1.1).ceil() as u32;
        vec![
            ("price_data[currency]".into(), "jpy".into()),
            ("price_data[product_data][name]".into(), item.name.clone()),
            ("price_data[unit_amount]".into(), unit_amount.to_string()),
            ("quantity".into(), item.qty.to_string()),
        ]
    }).collect();

    let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| "https://makimaki.fly.dev".into());
    let mut form_params: Vec<(String, String)> = vec![
        ("mode".into(), "payment".into()),
        ("success_url".into(), format!("{base_url}/api/stripe-success?order_id={id}")),
        ("cancel_url".into(), format!("{base_url}/#menu")),
        ("metadata[order_id]".into(), id.clone()),
    ];

    form_params.push(("line_items[0][price_data][currency]".into(), "jpy".into()));
    form_params.push(("line_items[0][price_data][product_data][name]".into(), "箱代 Box charge".into()));
    form_params.push(("line_items[0][price_data][unit_amount]".into(), "200".into()));
    form_params.push(("line_items[0][quantity]".into(), "1".into()));

    for (i, item_params) in line_items.iter().enumerate() {
        let idx = i + 1;
        for (k, v) in item_params {
            form_params.push((format!("line_items[{idx}][{k}]"), v.clone()));
        }
    }

    let client = reqwest::Client::new();
    match client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .basic_auth(&stripe_key, Option::<&str>::None)
        .form(&form_params)
        .send()
        .await
    {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            if let Some(url) = body["url"].as_str() {
                if let Some(sid) = body["id"].as_str() {
                    let db = state.db.lock().unwrap();
                    let _ = db.execute("UPDATE orders SET stripe_session=?1 WHERE id=?2", rusqlite::params![sid, id]);
                }
                (StatusCode::OK, Json(serde_json::json!({"url": url, "id": id})))
            } else {
                (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Stripe error", "detail": body.to_string()})))
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()}))),
    }
}

async fn stripe_success(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SuccessQuery>,
) -> impl IntoResponse {
    if let Some(order_id) = q.order_id {
        let db = state.db.lock().unwrap();
        let _ = db.execute("UPDATE orders SET paid=1 WHERE id=?1", rusqlite::params![order_id]);
    }
    Redirect::temporary("/?paid=1")
}

async fn list_orders(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!([])));
    }
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare("SELECT id,name,phone,pickup_time,items,note,total,status,created_at,paid FROM orders ORDER BY created_at DESC LIMIT 200")
        .unwrap();
    let orders: Vec<Order> = stmt
        .query_map([], |row| {
            let items_str: String = row.get(4)?;
            let items: Vec<OrderItem> = serde_json::from_str(&items_str).unwrap_or_default();
            Ok(Order {
                id: row.get(0)?,
                name: row.get(1)?,
                phone: row.get(2)?,
                pickup_time: row.get(3)?,
                items,
                note: row.get(5)?,
                total: row.get(6)?,
                status: row.get(7)?,
                paid: row.get::<_, i32>(9).unwrap_or(0) != 0,
                created_at: row.get(8)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    (StatusCode::OK, Json(serde_json::json!(orders)))
}

async fn update_order_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<UpdateStatus>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) { return StatusCode::UNAUTHORIZED; }
    let db = state.db.lock().unwrap();
    match db.execute("UPDATE orders SET status=?1 WHERE id=?2", rusqlite::params![input.status, id]) {
        Ok(n) if n > 0 => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}

async fn get_menu_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare("SELECT item_id, available FROM menu_status").unwrap();
    let items: Vec<MenuItem> = stmt.query_map([], |row| {
        Ok(MenuItem { id: row.get(0)?, available: row.get::<_, i32>(1)? != 0 })
    }).unwrap().filter_map(|r| r.ok()).collect();
    Json(items)
}

async fn update_menu_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(items): Json<Vec<MenuItem>>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) { return StatusCode::UNAUTHORIZED; }
    let db = state.db.lock().unwrap();
    for item in items {
        db.execute(
            "INSERT INTO menu_status (item_id, available) VALUES (?1, ?2) ON CONFLICT(item_id) DO UPDATE SET available=?2",
            rusqlite::params![item.id, item.available as i32],
        ).ok();
    }
    StatusCode::OK
}

async fn get_store_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let open: String = db.query_row("SELECT value FROM store_config WHERE key='open'", [], |r| r.get(0)).unwrap_or("1".into());
    let notice: String = db.query_row("SELECT value FROM store_config WHERE key='notice'", [], |r| r.get(0)).unwrap_or_default();
    Json(serde_json::json!({"open": open == "1", "notice": notice}))
}

async fn update_store_config(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<StoreStatus>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) { return StatusCode::UNAUTHORIZED; }
    let db = state.db.lock().unwrap();
    if let Some(open) = input.open {
        db.execute("UPDATE store_config SET value=?1 WHERE key='open'", rusqlite::params![if open {"1"} else {"0"}]).ok();
    }
    if let Some(notice) = input.notice {
        db.execute("UPDATE store_config SET value=?1 WHERE key='notice'", rusqlite::params![notice]).ok();
    }
    StatusCode::OK
}

async fn get_stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) { return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({}))); }
    let db = state.db.lock().unwrap();
    let today = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d")
        .to_string();
    let today_count: i32 = db.query_row(
        "SELECT COUNT(*) FROM orders WHERE created_at LIKE ?1||'%'", rusqlite::params![today], |r| r.get(0)
    ).unwrap_or(0);
    let today_revenue: i64 = db.query_row(
        "SELECT COALESCE(SUM(total),0) FROM orders WHERE created_at LIKE ?1||'%'", rusqlite::params![today], |r| r.get(0)
    ).unwrap_or(0);
    let total_orders: i32 = db.query_row("SELECT COUNT(*) FROM orders", [], |r| r.get(0)).unwrap_or(0);
    (StatusCode::OK, Json(serde_json::json!({
        "today_orders": today_count,
        "today_revenue": today_revenue,
        "total_orders": total_orders,
    })))
}

async fn admin_page() -> Html<&'static str> {
    Html(include_str!("../static/admin.html"))
}
