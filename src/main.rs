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
    square_token: Option<String>,
    square_location: Option<String>,
    admin_key: String,
    line_channel_token: Option<String>,
    line_channel_secret: Option<String>,
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
    printer_enabled: Option<bool>,
    line_channel_token: Option<String>,
    line_owner_user_id: Option<String>,
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
        );
        CREATE TABLE IF NOT EXISTS requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            body TEXT NOT NULL,
            author TEXT DEFAULT '',
            status TEXT DEFAULT 'open',
            created_at TEXT NOT NULL,
            resolved_at TEXT DEFAULT ''
        );
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event TEXT NOT NULL,
            data TEXT DEFAULT '',
            ts TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS inventory (
            date TEXT NOT NULL,
            slot TEXT NOT NULL,
            item_id TEXT NOT NULL,
            stock INTEGER DEFAULT 30,
            sold INTEGER DEFAULT 0,
            PRIMARY KEY (date, slot, item_id)
        );",
    )
    .expect("Failed to create tables");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_fixes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            body TEXT NOT NULL,
            status TEXT DEFAULT 'pending',
            issue_number INTEGER DEFAULT 0,
            cost TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );"
    ).ok();
    // Migrations (add columns if missing)
    conn.execute_batch("ALTER TABLE orders ADD COLUMN paid INTEGER DEFAULT 0").ok();
    conn.execute_batch("ALTER TABLE orders ADD COLUMN stripe_session TEXT DEFAULT ''").ok();
    conn.execute_batch("ALTER TABLE orders ADD COLUMN printed INTEGER DEFAULT 0").ok();
    // Default store config
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('open', '1')", []).ok();
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('notice', '')", []).ok();
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('printer_enabled', '0')", []).ok();
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('line_channel_token', '')", []).ok();
    conn.execute("INSERT OR IGNORE INTO store_config (key, value) VALUES ('line_owner_user_id', '')", []).ok();
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

    let square_token = std::env::var("SQUARE_ACCESS_TOKEN").ok();
    let square_location = std::env::var("SQUARE_LOCATION_ID").ok();
    let admin_key = std::env::var("ADMIN_KEY").expect("ADMIN_KEY environment variable must be set");
    let line_channel_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok().filter(|s| !s.is_empty());
    let line_channel_secret = std::env::var("LINE_CHANNEL_SECRET").ok().filter(|s| !s.is_empty());

    if square_token.is_some() { println!("Square payments enabled"); }
    if line_channel_token.is_some() { println!("LINE bot enabled"); }
    println!("Admin key configured");

    let state = Arc::new(AppState { db: Mutex::new(conn), square_token, square_location, admin_key, line_channel_token, line_channel_secret });

    let app = Router::new()
        .route("/api/orders", post(create_order).get(list_orders))
        .route("/api/orders/{id}/status", post(update_order_status))
        .route("/api/checkout", post(create_checkout))
        .route("/api/square-callback", get(square_callback))
        .route("/api/pos/order", post(create_pos_order))
        .route("/api/pos/terminal-checkout", post(create_terminal_checkout))
        .route("/api/pos/terminal-status/{checkout_id}", get(get_terminal_status))
        .route("/api/menu-status", get(get_menu_status).post(update_menu_status))
        .route("/api/store-config", get(get_store_config).post(update_store_config))
        .route("/api/stats", get(get_stats))
        .route("/api/auth", post(check_auth))
        .route("/api/requests", get(list_requests).post(create_request))
        .route("/api/requests/{id}/status", post(update_request_status))
        .route("/api/events", post(track_event).get(get_events))
        .route("/api/analytics", get(get_analytics))
        .route("/api/mv-analytics", get(get_mv_analytics))
        .route("/api/ai-fix", post(create_ai_fix))
        .route("/api/ai-fix-history", get(list_ai_fixes))
        .route("/api/cloudprnt", post(cloudprnt_poll))
        .route("/api/cloudprnt/job", get(cloudprnt_job))
        .route("/api/orders/{id}/cancel", post(cancel_order))
        .route("/api/square-webhook", post(square_webhook))
        .route("/api/line-webhook", post(line_webhook))
        .route("/api/inventory", get(get_inventory))
        .route("/pos", get(pos_page))
        .route("/order", get(order_page))
        .route("/box", get(|| async { Redirect::permanent("/#menu") }))
        .route("/docs", get(docs_page))
        .route("/admin", get(admin_page))
        .route("/guide", get(guide_page))
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

    let (line_token, line_uid) = {
        let db = state.db.lock().unwrap();
        let r = db.execute(
            "INSERT INTO orders (id,name,phone,pickup_time,items,note,total,status,paid,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'new',0,?8)",
            rusqlite::params![id, input.name, input.phone, input.pickup_time, items_json, input.note, total, now],
        );
        if let Err(e) = r {
            eprintln!("Error: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Server error"})));
        }
        let lt: String = db.query_row("SELECT value FROM store_config WHERE key='line_channel_token'", [], |r| r.get(0)).unwrap_or_default();
        let lu: String = db.query_row("SELECT value FROM store_config WHERE key='line_owner_user_id'", [], |r| r.get(0)).unwrap_or_default();
        (lt, lu)
    };

    // Send LINE notification (fire and forget)
    if !line_token.is_empty() && !line_uid.is_empty() {
        let msg = format!("🍣 新規注文 #{}\n{} 様\n受取: {}\n合計: ¥{}\n📞 {}", id, input.name, input.pickup_time, total, input.phone);
        let token = line_token.clone();
        let uid = line_uid.clone();
        tokio::spawn(async move {
            let _ = reqwest::Client::new()
                .post("https://api.line.me/v2/bot/message/push")
                .header("Authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({"to": uid, "messages": [{"type": "text", "text": msg}]}))
                .send().await;
        });
    }

    (StatusCode::CREATED, Json(serde_json::json!({"id": id, "total": total})))
}

async fn create_checkout(
    State(state): State<Arc<AppState>>,
    Json(input): Json<CreateOrder>,
) -> impl IntoResponse {
    let square_token = match &state.square_token {
        Some(k) => k.clone(),
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "Square not configured. Please use pay-at-shop."}))),
    };
    let location_id = state.square_location.clone().unwrap_or_default();

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let total: u32 = input.items.iter().map(|i| i.price * i.qty).sum();
    let total_with_tax = ((total as f64) * 1.08).ceil() as u32 + 200; // +箱代
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

    // Build Square line items
    let mut sq_line_items: Vec<serde_json::Value> = Vec::new();

    // Box charge
    sq_line_items.push(serde_json::json!({
        "name": "箱代 Box charge",
        "quantity": "1",
        "base_price_money": { "amount": 200, "currency": "JPY" }
    }));

    for item in &input.items {
        let unit_amount = ((item.price as f64) * 1.08).ceil() as u64;
        sq_line_items.push(serde_json::json!({
            "name": item.name,
            "quantity": item.qty.to_string(),
            "base_price_money": { "amount": unit_amount, "currency": "JPY" }
        }));
    }

    let base_url = std::env::var("BASE_URL").unwrap_or_else(|_| "https://makimaki.fly.dev".into());
    let idempotency_key = uuid::Uuid::new_v4().to_string();

    let payload = serde_json::json!({
        "idempotency_key": idempotency_key,
        "order": {
            "location_id": location_id,
            "line_items": sq_line_items,
            "metadata": { "order_id": id }
        },
        "checkout_options": {
            "redirect_url": format!("{base_url}/api/square-callback?order_id={id}")
        }
    });

    let client = reqwest::Client::new();
    match client
        .post("https://connect.squareup.com/v2/online-checkout/payment-links")
        .header("Authorization", format!("Bearer {square_token}"))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
    {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            if let Some(url) = body["payment_link"]["url"].as_str() {
                let sq_order_id = body["payment_link"]["order_id"].as_str().unwrap_or("");
                let db = state.db.lock().unwrap();
                let _ = db.execute("UPDATE orders SET stripe_session=?1 WHERE id=?2", rusqlite::params![sq_order_id, id]);
                (StatusCode::OK, Json(serde_json::json!({"url": url, "id": id})))
            } else {
                eprintln!("Square error: {body}");
                (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Payment processing failed"})))
            }
        }
        Err(e) => {
            eprintln!("Error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Server error"})))
        }
    }
}

async fn square_callback(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SuccessQuery>,
) -> impl IntoResponse {
    if let Some(order_id) = &q.order_id {
        // Verify payment via Square Orders API
        let verified = if let Some(square_token) = &state.square_token {
            let sq_order_id: Option<String> = {
                let db = state.db.lock().unwrap();
                db.query_row(
                    "SELECT stripe_session FROM orders WHERE id=?1",
                    rusqlite::params![order_id], |r| r.get(0)
                ).ok()
            };
            if let Some(sq_id) = sq_order_id.filter(|s| !s.is_empty()) {
                let client = reqwest::Client::new();
                if let Ok(resp) = client
                    .get(&format!("https://connect.squareup.com/v2/orders/{sq_id}"))
                    .header("Authorization", format!("Bearer {square_token}"))
                    .send().await
                {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        let state_str = body["order"]["state"].as_str().unwrap_or("");
                        state_str == "COMPLETED" || state_str == "OPEN"
                    } else { false }
                } else { false }
            } else {
                // No Square order ID stored — trust the redirect (payment link only redirects on success)
                true
            }
        } else { false };

        if verified {
            let db = state.db.lock().unwrap();
            let _ = db.execute("UPDATE orders SET paid=1 WHERE id=?1", rusqlite::params![order_id]);
        }
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
    let printer: String = db.query_row("SELECT value FROM store_config WHERE key='printer_enabled'", [], |r| r.get(0)).unwrap_or("0".into());
    let line_token: String = db.query_row("SELECT value FROM store_config WHERE key='line_channel_token'", [], |r| r.get(0)).unwrap_or_default();
    let line_uid: String = db.query_row("SELECT value FROM store_config WHERE key='line_owner_user_id'", [], |r| r.get(0)).unwrap_or_default();
    Json(serde_json::json!({"open": open == "1", "notice": notice, "printer_enabled": printer == "1",
        "line_token_set": !line_token.is_empty(), "line_uid_set": !line_uid.is_empty()}))
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
    if let Some(printer) = input.printer_enabled {
        db.execute("INSERT INTO store_config (key, value) VALUES ('printer_enabled', ?1) ON CONFLICT(key) DO UPDATE SET value=?1",
            rusqlite::params![if printer {"1"} else {"0"}]).ok();
    }
    if let Some(ref token) = input.line_channel_token {
        db.execute("INSERT INTO store_config (key, value) VALUES ('line_channel_token', ?1) ON CONFLICT(key) DO UPDATE SET value=?1",
            rusqlite::params![token]).ok();
    }
    if let Some(ref uid) = input.line_owner_user_id {
        db.execute("INSERT INTO store_config (key, value) VALUES ('line_owner_user_id', ?1) ON CONFLICT(key) DO UPDATE SET value=?1",
            rusqlite::params![uid]).ok();
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

async fn guide_page(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let key = q.get("key").map(|s| s.as_str()).unwrap_or("");
    if key == state.admin_key {
        Html(include_str!("../static/guide.html")).into_response()
    } else {
        Html("<html><body style='font-family:sans-serif;display:flex;height:100vh;align-items:center;justify-content:center'><div style='text-align:center'><h2>makimaki ガイド</h2><p style='color:#999;margin:1rem 0'>認証が必要です。URLに ?key=xxx を付けてアクセスしてください。</p></div></body></html>").into_response()
    }
}

// --- Requests (feature requests / feedback) ---

#[derive(Serialize)]
struct FeatureRequest {
    id: i64,
    body: String,
    author: String,
    status: String,
    created_at: String,
    resolved_at: String,
}

#[derive(Deserialize)]
struct CreateRequest {
    body: String,
    author: String,
}

#[derive(Deserialize)]
struct UpdateRequestStatus {
    status: String,
}

async fn list_requests(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare("SELECT id,body,author,status,created_at,resolved_at FROM requests ORDER BY id DESC LIMIT 100").unwrap();
    let reqs: Vec<FeatureRequest> = stmt.query_map([], |row| {
        Ok(FeatureRequest {
            id: row.get(0)?, body: row.get(1)?, author: row.get(2)?,
            status: row.get(3)?, created_at: row.get(4)?, resolved_at: row.get::<_,String>(5).unwrap_or_default(),
        })
    }).unwrap().filter_map(|r| r.ok()).collect();
    Json(reqs)
}

async fn create_request(
    State(state): State<Arc<AppState>>,
    Json(input): Json<CreateRequest>,
) -> impl IntoResponse {
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M").to_string();
    let db = state.db.lock().unwrap();
    match db.execute(
        "INSERT INTO requests (body, author, status, created_at) VALUES (?1, ?2, 'open', ?3)",
        rusqlite::params![input.body, input.author, now],
    ) {
        Ok(_) => StatusCode::CREATED,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn update_request_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(input): Json<UpdateRequestStatus>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) { return StatusCode::UNAUTHORIZED; }
    let db = state.db.lock().unwrap();
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M").to_string();
    let resolved = if input.status == "resolved" { &now } else { "" };
    match db.execute(
        "UPDATE requests SET status=?1, resolved_at=?2 WHERE id=?3",
        rusqlite::params![input.status, resolved, id],
    ) {
        Ok(n) if n > 0 => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}

// --- Analytics Events ---

#[derive(Deserialize)]
struct TrackEvent {
    event: String,
    data: Option<String>,
}

async fn track_event(
    State(state): State<Arc<AppState>>,
    Json(input): Json<TrackEvent>,
) -> impl IntoResponse {
    // Whitelist allowed events to prevent abuse
    const ALLOWED: &[&str] = &[
        "pageview", "scroll_25", "scroll_50", "scroll_75", "scroll_100",
        "section_view", "cart_add", "checkout_start", "checkout_submit",
        "square_redirect", "order_nopay", "order_complete",
        "box_select", "pos_order",
        "mv_shown", "mv_play", "mv_pause", "mv_complete", "mv_click", "ab_group",
    ];
    if !ALLOWED.contains(&input.event.as_str()) {
        return StatusCode::BAD_REQUEST;
    }
    let data = input.data.unwrap_or_default();
    if data.len() > 200 { return StatusCode::BAD_REQUEST; }
    // Skip admin events (data starts with "admin:")
    if data.starts_with("admin:") { return StatusCode::OK; }

    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M:%S").to_string();
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "INSERT INTO events (event, data, ts) VALUES (?1, ?2, ?3)",
        rusqlite::params![input.event, data, now],
    );
    StatusCode::OK
}

async fn get_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({})));
    }
    let db = state.db.lock().unwrap();
    let today = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d").to_string();

    let mut counts: std::collections::HashMap<String, i32> = std::collections::HashMap::new();
    let mut stmt = db.prepare("SELECT event, COUNT(*) FROM events WHERE ts LIKE ?1||'%' GROUP BY event").unwrap();
    let rows = stmt.query_map(rusqlite::params![today], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
    }).unwrap();
    for row in rows.flatten() {
        counts.insert(row.0, row.1);
    }

    // Funnel: recent cart items
    let mut cart_items: Vec<String> = Vec::new();
    let mut stmt2 = db.prepare("SELECT data FROM events WHERE event='cart_add' AND ts LIKE ?1||'%' ORDER BY id DESC LIMIT 50").unwrap();
    let rows2 = stmt2.query_map(rusqlite::params![today], |row| row.get::<_, String>(0)).unwrap();
    for row in rows2.flatten() {
        cart_items.push(row);
    }

    (StatusCode::OK, Json(serde_json::json!({
        "today": counts,
        "recent_cart": cart_items,
    })))
}

async fn get_analytics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({})));
    }
    let db = state.db.lock().unwrap();

    // Daily PV for last 30 days
    let mut daily: Vec<serde_json::Value> = Vec::new();
    let mut stmt = db.prepare(
        "SELECT SUBSTR(ts,1,10) as day, event, COUNT(*) FROM events WHERE ts >= date('now','-30 days') GROUP BY day, event ORDER BY day"
    ).unwrap();
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,i32>(2)?))
    }).unwrap();

    let mut day_map: std::collections::BTreeMap<String, std::collections::HashMap<String, i32>> = std::collections::BTreeMap::new();
    for row in rows.flatten() {
        day_map.entry(row.0).or_default().insert(row.1, row.2);
    }
    for (day, events) in &day_map {
        daily.push(serde_json::json!({
            "date": day,
            "pageview": events.get("pageview").unwrap_or(&0),
            "cart_add": events.get("cart_add").unwrap_or(&0),
            "checkout_start": events.get("checkout_start").unwrap_or(&0),
            "order_complete": events.get("order_complete").unwrap_or(&0),
            "stripe_redirect": events.get("stripe_redirect").unwrap_or(&0),
        }));
    }

    // Daily orders + revenue
    let mut order_daily: Vec<serde_json::Value> = Vec::new();
    let mut stmt2 = db.prepare(
        "SELECT SUBSTR(created_at,1,10) as day, COUNT(*), COALESCE(SUM(total),0), SUM(CASE WHEN paid=1 THEN 1 ELSE 0 END) FROM orders WHERE created_at >= date('now','-30 days') GROUP BY day ORDER BY day"
    ).unwrap();
    let rows2 = stmt2.query_map([], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,i32>(1)?, row.get::<_,i64>(2)?, row.get::<_,i32>(3)?))
    }).unwrap();
    for row in rows2.flatten() {
        order_daily.push(serde_json::json!({"date":row.0,"orders":row.1,"revenue":row.2,"paid":row.3}));
    }

    // Popular items all-time
    let mut popular: Vec<serde_json::Value> = Vec::new();
    let mut stmt3 = db.prepare("SELECT data, COUNT(*) as cnt FROM events WHERE event='cart_add' AND data!='' GROUP BY data ORDER BY cnt DESC LIMIT 20").unwrap();
    let rows3 = stmt3.query_map([], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,i32>(1)?))
    }).unwrap();
    for row in rows3.flatten() {
        popular.push(serde_json::json!({"item":row.0,"count":row.1}));
    }

    (StatusCode::OK, Json(serde_json::json!({
        "daily_events": daily,
        "daily_orders": order_daily,
        "popular_items": popular,
    })))
}

// --- MV A/B Test Analytics ---
async fn get_mv_analytics(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({})));
    }
    let db = state.db.lock().unwrap();

    // Group A (MV shown) vs Group B (no MV) conversion comparison
    // Count users in each group
    let group_a: i32 = db.query_row(
        "SELECT COUNT(DISTINCT data) FROM events WHERE event='ab_group' AND data='A'", [], |r| r.get(0)
    ).unwrap_or(0);
    let group_b: i32 = db.query_row(
        "SELECT COUNT(DISTINCT data) FROM events WHERE event='ab_group' AND data='B'", [], |r| r.get(0)
    ).unwrap_or(0);

    // MV events
    let mv_shown: i32 = db.query_row("SELECT COUNT(*) FROM events WHERE event='mv_shown'", [], |r| r.get(0)).unwrap_or(0);
    let mv_play: i32 = db.query_row("SELECT COUNT(*) FROM events WHERE event='mv_play'", [], |r| r.get(0)).unwrap_or(0);
    let mv_pause: i32 = db.query_row("SELECT COUNT(*) FROM events WHERE event='mv_pause'", [], |r| r.get(0)).unwrap_or(0);
    let mv_complete: i32 = db.query_row("SELECT COUNT(*) FROM events WHERE event='mv_complete'", [], |r| r.get(0)).unwrap_or(0);
    let mv_click: i32 = db.query_row("SELECT COUNT(*) FROM events WHERE event='mv_click'", [], |r| r.get(0)).unwrap_or(0);

    // Listen durations from mv_pause and mv_complete data
    let mut listen_times: Vec<i32> = Vec::new();
    let mut stmt = db.prepare("SELECT data FROM events WHERE event IN ('mv_pause','mv_complete') AND data LIKE '%s'").unwrap();
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
    for row in rows.flatten() {
        if let Ok(secs) = row.trim_end_matches('s').parse::<i32>() {
            listen_times.push(secs);
        }
    }
    let avg_listen = if listen_times.is_empty() { 0 } else { listen_times.iter().sum::<i32>() / listen_times.len() as i32 };
    let max_listen = listen_times.iter().max().copied().unwrap_or(0);

    // Checkout events per group (approximate by timestamp correlation)
    // Group A checkout = sessions with both mv_shown and checkout_start on same day
    let a_checkouts: i32 = db.query_row(
        "SELECT COUNT(*) FROM events WHERE event='checkout_start' AND SUBSTR(ts,1,10) IN (SELECT DISTINCT SUBSTR(ts,1,10) FROM events WHERE event='mv_shown')",
        [], |r| r.get(0)
    ).unwrap_or(0);
    let b_checkouts: i32 = db.query_row(
        "SELECT COUNT(*) FROM events WHERE event='checkout_start' AND SUBSTR(ts,1,10) NOT IN (SELECT DISTINCT SUBSTR(ts,1,10) FROM events WHERE event='mv_shown')",
        [], |r| r.get(0)
    ).unwrap_or(0);

    // Daily MV metrics
    let mut daily_mv: Vec<serde_json::Value> = Vec::new();
    let mut stmt2 = db.prepare(
        "SELECT SUBSTR(ts,1,10) as day, event, COUNT(*) FROM events WHERE event IN ('mv_shown','mv_play','mv_complete','mv_click','ab_group') GROUP BY day, event ORDER BY day"
    ).unwrap();
    let rows2 = stmt2.query_map([], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,i32>(2)?))
    }).unwrap();
    let mut day_map: std::collections::BTreeMap<String, std::collections::HashMap<String, i32>> = std::collections::BTreeMap::new();
    for row in rows2.flatten() {
        day_map.entry(row.0).or_default().insert(row.1, row.2);
    }
    for (day, events) in &day_map {
        daily_mv.push(serde_json::json!({
            "date": day,
            "shown": events.get("mv_shown").unwrap_or(&0),
            "play": events.get("mv_play").unwrap_or(&0),
            "complete": events.get("mv_complete").unwrap_or(&0),
            "click": events.get("mv_click").unwrap_or(&0),
        }));
    }

    (StatusCode::OK, Json(serde_json::json!({
        "ab_test": {
            "group_a_users": group_a,
            "group_b_users": group_b,
            "group_a_checkouts": a_checkouts,
            "group_b_checkouts": b_checkouts,
            "group_a_cv_rate": if group_a > 0 { format!("{:.1}%", a_checkouts as f64 / group_a as f64 * 100.0) } else { "0%".to_string() },
            "group_b_cv_rate": if group_b > 0 { format!("{:.1}%", b_checkouts as f64 / group_b as f64 * 100.0) } else { "0%".to_string() },
        },
        "mv_engagement": {
            "shown": mv_shown,
            "play": mv_play,
            "pause": mv_pause,
            "complete": mv_complete,
            "mv_page_click": mv_click,
            "play_rate": if mv_shown > 0 { format!("{:.1}%", mv_play as f64 / mv_shown as f64 * 100.0) } else { "0%".to_string() },
            "completion_rate": if mv_play > 0 { format!("{:.1}%", mv_complete as f64 / mv_play as f64 * 100.0) } else { "0%".to_string() },
            "avg_listen_seconds": avg_listen,
            "max_listen_seconds": max_listen,
        },
        "daily": daily_mv,
    })))
}

// --- Star CloudPRNT ---
// The printer POSTs to /api/cloudprnt periodically.
// If there's an unprinted order, respond with jobReady=true.
// Printer then GETs /api/cloudprnt/job to fetch receipt data.

async fn cloudprnt_poll(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let db = state.db.lock().unwrap();

    // Check if printer is enabled
    let enabled: String = db.query_row(
        "SELECT value FROM store_config WHERE key='printer_enabled'", [], |r| r.get(0)
    ).unwrap_or("0".into());
    if enabled != "1" {
        return Json(serde_json::json!({"jobReady": false}));
    }

    // Check for unprinted orders
    let has_unprinted: bool = db.query_row(
        "SELECT COUNT(*) FROM orders WHERE printed=0 AND status='new'", [], |r| r.get::<_,i32>(0)
    ).unwrap_or(0) > 0;

    Json(serde_json::json!({
        "jobReady": has_unprinted,
        "mediaTypes": ["text/plain"]
    }))
}

async fn cloudprnt_job(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let db = state.db.lock().unwrap();

    // Get oldest unprinted order
    let result = db.query_row(
        "SELECT id,name,phone,pickup_time,items,note,total,paid,created_at FROM orders WHERE printed=0 AND status='new' ORDER BY created_at ASC LIMIT 1",
        [],
        |row| {
            let items_str: String = row.get(4)?;
            Ok((
                row.get::<_,String>(0)?,  // id
                row.get::<_,String>(1)?,  // name
                row.get::<_,String>(2)?,  // phone
                row.get::<_,String>(3)?,  // pickup_time
                items_str,
                row.get::<_,String>(5)?,  // note
                row.get::<_,u32>(6)?,     // total
                row.get::<_,i32>(7)?,     // paid
                row.get::<_,String>(8)?,  // created_at
            ))
        }
    );

    match result {
        Ok((id, name, phone, pickup_time, items_str, note, total, paid, created_at)) => {
            let items: Vec<OrderItem> = serde_json::from_str(&items_str).unwrap_or_default();

            // Mark as printed
            let _ = db.execute("UPDATE orders SET printed=1 WHERE id=?1", rusqlite::params![id]);

            // Generate receipt text (Star line mode compatible)
            let mut receipt = String::new();
            receipt.push_str("================================\n");
            receipt.push_str("        makimaki\n");
            receipt.push_str("     細巻き専門店\n");
            receipt.push_str("================================\n\n");
            receipt.push_str(&format!("注文番号: #{}\n", id));
            receipt.push_str(&format!("受付時間: {}\n", created_at));
            receipt.push_str(&format!("受取時間: {}\n\n", pickup_time));
            receipt.push_str(&format!("お名前: {}\n", name));
            receipt.push_str(&format!("電話: {}\n\n", phone));
            receipt.push_str("--------------------------------\n");

            for item in &items {
                let line_total = item.price * item.qty;
                receipt.push_str(&format!(
                    "{} x{}\n    ¥{}\n",
                    item.name, item.qty, line_total
                ));
            }

            receipt.push_str("--------------------------------\n");
            receipt.push_str(&format!("合計: ¥{}\n", total));
            receipt.push_str(&format!("決済: {}\n\n", if paid != 0 { "カード済" } else { "未払い" }));

            if !note.is_empty() {
                // Format box info nicely if present
                let display_note = note.replace(" / ", "\n  ");
                receipt.push_str(&format!("備考:\n  {}\n\n", display_note));
            }

            receipt.push_str("================================\n");
            receipt.push_str("   ありがとうございます\n");
            receipt.push_str("================================\n\n\n\n");

            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                receipt,
            ).into_response()
        }
        Err(_) => {
            (StatusCode::NO_CONTENT, "").into_response()
        }
    }
}

// --- Cancel & Refund ---
async fn cancel_order(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) { return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"}))); }

    let (sq_order_id, paid) = {
        let db = state.db.lock().unwrap();
        let result = db.query_row(
            "SELECT stripe_session, paid FROM orders WHERE id=?1",
            rusqlite::params![id], |r| Ok((r.get::<_,String>(0)?, r.get::<_,i32>(1)?))
        );
        match result {
            Ok(r) => r,
            Err(_) => return (StatusCode::NOT_FOUND, Json(serde_json::json!({"error":"order not found"}))),
        }
    };

    // If paid via Square, attempt refund
    let mut refunded = false;
    if paid != 0 && !sq_order_id.is_empty() {
        if let Some(square_token) = &state.square_token {
            let client = reqwest::Client::new();
            // Get payments for this order
            if let Ok(resp) = client
                .get(&format!("https://connect.squareup.com/v2/orders/{sq_order_id}"))
                .header("Authorization", format!("Bearer {square_token}"))
                .send().await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    // Get tender/payment IDs
                    if let Some(tenders) = body["order"]["tenders"].as_array() {
                        for tender in tenders {
                            if let Some(payment_id) = tender["payment_id"].as_str() {
                                let refund_payload = serde_json::json!({
                                    "idempotency_key": uuid::Uuid::new_v4().to_string(),
                                    "payment_id": payment_id,
                                    "amount_money": tender["amount_money"],
                                    "reason": "Order cancelled"
                                });
                                let _ = client
                                    .post("https://connect.squareup.com/v2/refunds")
                                    .header("Authorization", format!("Bearer {square_token}"))
                                    .json(&refund_payload)
                                    .send().await;
                                refunded = true;
                            }
                        }
                    }
                }
            }
        }
    }

    let db = state.db.lock().unwrap();
    let _ = db.execute("UPDATE orders SET status='cancelled', paid=0 WHERE id=?1", rusqlite::params![id]);
    (StatusCode::OK, Json(serde_json::json!({"ok": true, "refunded": refunded})))
}

// --- Square Webhook ---
async fn square_webhook(
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
    let event: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let event_type = event["type"].as_str().unwrap_or("");
    // payment.completed or order.updated
    if event_type == "payment.completed" {
        if let Some(order_id) = event["data"]["object"]["payment"]["order_id"].as_str() {
            // Look up our internal order by square order id
            let db = state.db.lock().unwrap();
            let _ = db.execute("UPDATE orders SET paid=1 WHERE stripe_session=?1", rusqlite::params![order_id]);
        }
    }
    StatusCode::OK
}

// --- POS Order (in-store, cash or card) ---
#[derive(Deserialize)]
struct PosOrder {
    items: Vec<OrderItem>,
    note: String,
    payment_method: String, // "cash", "card", "qr"
    customer_name: Option<String>,
}

async fn create_pos_order(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<PosOrder>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"})));
    }

    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let total: u32 = input.items.iter().map(|i| i.price * i.qty).sum();
    let total_with_tax = ((total as f64) * 1.08).ceil() as u32 + 200;
    let items_json = serde_json::to_string(&input.items).unwrap_or_default();
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M")
        .to_string();
    let name = input.customer_name.unwrap_or_else(|| "店頭".into());
    let note = format!("[{}] {}", input.payment_method.to_uppercase(), input.note);

    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "INSERT INTO orders (id,name,phone,pickup_time,items,note,total,status,paid,created_at) VALUES (?1,?2,'店頭','店頭受取',?3,?4,?5,'new',1,?6)",
        rusqlite::params![id, name, items_json, note, total_with_tax, now],
    );

    (StatusCode::CREATED, Json(serde_json::json!({"id": id, "total": total_with_tax})))
}

// --- Square Terminal Checkout ---
#[derive(Deserialize)]
struct TerminalCheckoutRequest {
    items: Vec<OrderItem>,
    note: String,
    device_id: Option<String>,
}

async fn create_terminal_checkout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<TerminalCheckoutRequest>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"})));
    }
    let square_token = match &state.square_token {
        Some(k) => k.clone(),
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"Square not configured"}))),
    };

    let order_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let total: u32 = input.items.iter().map(|i| i.price * i.qty).sum();
    let total_with_tax = ((total as f64) * 1.08).ceil() as u32 + 200;
    let items_json = serde_json::to_string(&input.items).unwrap_or_default();
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M")
        .to_string();

    // Insert order as unpaid first
    {
        let db = state.db.lock().unwrap();
        let _ = db.execute(
            "INSERT INTO orders (id,name,phone,pickup_time,items,note,total,status,paid,created_at) VALUES (?1,'店頭','店頭','店頭受取',?2,?3,?4,'new',0,?5)",
            rusqlite::params![order_id, items_json, format!("[CARD] {}", input.note), total_with_tax, now],
        );
    }

    let location_id = state.square_location.clone().unwrap_or_default();
    let idempotency_key = uuid::Uuid::new_v4().to_string();

    // Build the terminal checkout payload
    let mut checkout_payload = serde_json::json!({
        "idempotency_key": idempotency_key,
        "checkout": {
            "amount_money": {
                "amount": total_with_tax as u64,
                "currency": "JPY"
            },
            "reference_id": order_id,
            "note": format!("makimaki #{}", order_id),
            "device_options": {
                "device_id": input.device_id.as_deref().unwrap_or(""),
                "skip_receipt_screen": false,
                "collect_signature": false
            },
            "payment_type": "CARD_PRESENT"
        }
    });

    // If no device_id provided, try to get the first available terminal
    if input.device_id.is_none() || input.device_id.as_deref() == Some("") {
        // List device codes to find the terminal
        let client = reqwest::Client::new();
        if let Ok(resp) = client
            .get("https://connect.squareup.com/v2/devices")
            .header("Authorization", format!("Bearer {square_token}"))
            .send().await
        {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(devices) = body["devices"].as_array() {
                    if let Some(device) = devices.first() {
                        if let Some(did) = device["id"].as_str() {
                            checkout_payload["checkout"]["device_options"]["device_id"] = serde_json::Value::String(did.to_string());
                        }
                    }
                }
            }
        }
    }

    let client = reqwest::Client::new();
    match client
        .post("https://connect.squareup.com/v2/terminals/checkouts")
        .header("Authorization", format!("Bearer {square_token}"))
        .header("Content-Type", "application/json")
        .json(&checkout_payload)
        .send()
        .await
    {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            if let Some(checkout) = body.get("checkout") {
                let checkout_id = checkout["id"].as_str().unwrap_or("");
                let status = checkout["status"].as_str().unwrap_or("PENDING");
                // Store checkout ID in stripe_session field for tracking
                let db = state.db.lock().unwrap();
                let _ = db.execute("UPDATE orders SET stripe_session=?1 WHERE id=?2",
                    rusqlite::params![checkout_id, order_id]);
                (StatusCode::OK, Json(serde_json::json!({
                    "order_id": order_id,
                    "checkout_id": checkout_id,
                    "status": status,
                    "total": total_with_tax
                })))
            } else {
                eprintln!("Square Terminal error: {body}");
                // Clean up the unpaid order
                let db = state.db.lock().unwrap();
                let _ = db.execute("DELETE FROM orders WHERE id=?1", rusqlite::params![order_id]);
                (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Terminal checkout failed", "detail": body.to_string()})))
            }
        }
        Err(e) => {
            eprintln!("Terminal error: {e}");
            let db = state.db.lock().unwrap();
            let _ = db.execute("DELETE FROM orders WHERE id=?1", rusqlite::params![order_id]);
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error":"Server error"})))
        }
    }
}

async fn get_terminal_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(checkout_id): Path<String>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"})));
    }
    let square_token = match &state.square_token {
        Some(k) => k.clone(),
        None => return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"Square not configured"}))),
    };

    let client = reqwest::Client::new();
    match client
        .get(&format!("https://connect.squareup.com/v2/terminals/checkouts/{checkout_id}"))
        .header("Authorization", format!("Bearer {square_token}"))
        .send().await
    {
        Ok(resp) => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let status = body["checkout"]["status"].as_str().unwrap_or("UNKNOWN");
            let payment_ids = body["checkout"]["payment_ids"].as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default();

            // If completed, mark order as paid
            if status == "COMPLETED" {
                let db = state.db.lock().unwrap();
                let _ = db.execute("UPDATE orders SET paid=1 WHERE stripe_session=?1",
                    rusqlite::params![checkout_id]);
            }

            (StatusCode::OK, Json(serde_json::json!({
                "status": status,
                "payment_ids": payment_ids,
            })))
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": e.to_string()})))
        }
    }
}

// --- Inventory ---
async fn get_inventory(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let today = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d").to_string();
    let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1))
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d").to_string();

    // Auto-create slots if not exist for today and tomorrow
    let items = ["makimaki_box", "maguro_box"];
    let slots = ["11:00","11:30","12:00","12:30","13:00","13:30","14:00","14:30","15:00","15:30","16:00","16:30","17:00","17:30","18:00"];
    for date in [&today, &tomorrow] {
        for slot in &slots {
            for item in &items {
                db.execute(
                    "INSERT OR IGNORE INTO inventory (date,slot,item_id,stock,sold) VALUES (?1,?2,?3,30,0)",
                    rusqlite::params![date, slot, item],
                ).ok();
            }
        }
    }

    let mut stmt = db.prepare(
        "SELECT date,slot,item_id,stock,sold FROM inventory WHERE date IN (?1,?2) ORDER BY date,slot"
    ).unwrap();
    let rows: Vec<serde_json::Value> = stmt.query_map(
        rusqlite::params![today, tomorrow], |row| {
            Ok(serde_json::json!({
                "date": row.get::<_,String>(0)?,
                "slot": row.get::<_,String>(1)?,
                "item_id": row.get::<_,String>(2)?,
                "stock": row.get::<_,i32>(3)?,
                "sold": row.get::<_,i32>(4)?,
                "remaining": row.get::<_,i32>(3)? - row.get::<_,i32>(4)?,
            }))
        }
    ).unwrap().filter_map(|r| r.ok()).collect();
    Json(serde_json::json!(rows))
}

async fn pos_page(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let key = q.get("key").map(|s| s.as_str()).unwrap_or("");
    if key == state.admin_key {
        Html(include_str!("../static/pos.html")).into_response()
    } else {
        Html("<html><body style='font-family:sans-serif;display:flex;height:100vh;align-items:center;justify-content:center'><div style='text-align:center'><h2>makimaki POS</h2><p style='color:#999'>認証が必要です。URLに ?key=xxx を付けてアクセスしてください。</p></div></body></html>").into_response()
    }
}

async fn order_page() -> Html<&'static str> {
    Html(include_str!("../static/order.html"))
}

// --- AI Fix (GitHub Issue creation) ---
#[derive(Deserialize)]
struct AiFixRequest { body: String }

async fn create_ai_fix(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(input): Json<AiFixRequest>,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error":"unauthorized"})));
    }
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M").to_string();

    // Save to DB
    let fix_id = {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO ai_fixes (body, status, created_at) VALUES (?1, 'pending', ?2)",
            rusqlite::params![input.body, now],
        ).ok();
        db.last_insert_rowid()
    };

    // Create GitHub Issue via API
    let gh_token = std::env::var("GITHUB_TOKEN").ok();
    let mut issue_number = 0i64;
    if let Some(token) = gh_token {
        let client = reqwest::Client::new();
        let resp = client.post("https://api.github.com/repos/yukihamada/makimaki/issues")
            .header("Authorization", format!("Bearer {token}"))
            .header("User-Agent", "makimaki-bot")
            .json(&serde_json::json!({
                "title": format!("[AI修正] {}", if input.body.len() > 50 { &input.body[..50] } else { &input.body }),
                "body": format!("## 修正依頼\n\n{}\n\n---\n管理画面から送信 (fix_id: {})", input.body, fix_id),
                "labels": ["fix-request"]
            }))
            .send().await;
        if let Ok(r) = resp {
            if let Ok(body) = r.json::<serde_json::Value>().await {
                issue_number = body["number"].as_i64().unwrap_or(0);
                let db = state.db.lock().unwrap();
                db.execute("UPDATE ai_fixes SET issue_number=?1 WHERE id=?2",
                    rusqlite::params![issue_number, fix_id]).ok();
            }
        }
    }

    (StatusCode::OK, Json(serde_json::json!({"ok": true, "fix_id": fix_id, "issue_number": issue_number})))
}

async fn list_ai_fixes(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !check_admin(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!([])));
    }
    let db = state.db.lock().unwrap();
    let mut stmt = db.prepare("SELECT id, body, status, issue_number, cost, created_at FROM ai_fixes ORDER BY id DESC LIMIT 50").unwrap();
    let fixes: Vec<serde_json::Value> = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_,i64>(0)?,
            "body": row.get::<_,String>(1)?,
            "status": row.get::<_,String>(2)?,
            "issue_number": row.get::<_,i64>(3)?,
            "cost": row.get::<_,String>(4)?,
            "created_at": row.get::<_,String>(5)?,
        }))
    }).unwrap().filter_map(|r| r.ok()).collect();
    (StatusCode::OK, Json(serde_json::json!(fixes)))
}

async fn docs_page(
    State(state): State<Arc<AppState>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let key = q.get("key").map(|s| s.as_str()).unwrap_or("");
    if key == state.admin_key {
        Html(include_str!("../static/_docs.html")).into_response()
    } else {
        Html("<html><body style='font-family:sans-serif;display:flex;height:100vh;align-items:center;justify-content:center'><div style='text-align:center'><h2>makimaki docs</h2><p style='color:#999;margin:1rem 0'>認証が必要です。URLに ?key=xxx を付けてアクセスしてください。</p></div></body></html>").into_response()
    }
}

// --- LINE Messaging API Webhook ---
async fn line_webhook(
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
    let token = match &state.line_channel_token {
        Some(t) => t.clone(),
        None => return StatusCode::OK, // silently accept if not configured
    };

    let payload: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let events = match payload["events"].as_array() {
        Some(e) => e.clone(),
        None => return StatusCode::OK,
    };

    let client = reqwest::Client::new();

    for event in &events {
        let event_type = event["type"].as_str().unwrap_or("");

        match event_type {
            "follow" => {
                // New friend added — send welcome message
                if let Some(reply_token) = event["replyToken"].as_str() {
                    let welcome = serde_json::json!({
                        "replyToken": reply_token,
                        "messages": [
                            {
                                "type": "flex",
                                "altText": "makimakiへようこそ！",
                                "contents": line_welcome_flex()
                            }
                        ]
                    });
                    let _ = client
                        .post("https://api.line.me/v2/bot/message/reply")
                        .header("Authorization", format!("Bearer {token}"))
                        .json(&welcome)
                        .send().await;
                }
            }
            "message" => {
                let reply_token = match event["replyToken"].as_str() {
                    Some(t) => t,
                    None => continue,
                };
                let msg_type = event["message"]["type"].as_str().unwrap_or("");
                let text = event["message"]["text"].as_str().unwrap_or("").to_lowercase();

                let reply = if msg_type == "text" && (text.contains("メニュー") || text.contains("menu")) {
                    serde_json::json!({
                        "replyToken": reply_token,
                        "messages": [{
                            "type": "flex",
                            "altText": "makimaki メニュー",
                            "contents": line_menu_flex()
                        }]
                    })
                } else if msg_type == "text" && (text.contains("注文") || text.contains("order") || text.contains("予約")) {
                    serde_json::json!({
                        "replyToken": reply_token,
                        "messages": [{
                            "type": "flex",
                            "altText": "オンライン注文",
                            "contents": line_order_flex()
                        }]
                    })
                } else if msg_type == "text" && (text.contains("アクセス") || text.contains("場所") || text.contains("行き方") || text.contains("access") || text.contains("map")) {
                    serde_json::json!({
                        "replyToken": reply_token,
                        "messages": [{
                            "type": "flex",
                            "altText": "makimaki アクセス",
                            "contents": line_access_flex()
                        }]
                    })
                } else if msg_type == "text" && (text.contains("営業") || text.contains("時間") || text.contains("hours") || text.contains("open")) {
                    serde_json::json!({
                        "replyToken": reply_token,
                        "messages": [{
                            "type": "text",
                            "text": "🕚 営業時間\n\n11:00 - 18:00\n定休日: 不定休\n\n📍 東京都港区麻布十番2-19-1\nオフィスエイトビル 1F\n（麻布十番駅 徒歩1分）"
                        }]
                    })
                } else {
                    // Default: show quick reply with options
                    serde_json::json!({
                        "replyToken": reply_token,
                        "messages": [{
                            "type": "text",
                            "text": "makimakiへようこそ！🍣\n下のメニューからお選びください。",
                            "quickReply": {
                                "items": [
                                    {"type":"action","action":{"type":"message","label":"📋 メニュー","text":"メニュー"}},
                                    {"type":"action","action":{"type":"message","label":"🛒 注文する","text":"注文"}},
                                    {"type":"action","action":{"type":"message","label":"📍 アクセス","text":"アクセス"}},
                                    {"type":"action","action":{"type":"message","label":"🕚 営業時間","text":"営業時間"}}
                                ]
                            }
                        }]
                    })
                };

                let _ = client
                    .post("https://api.line.me/v2/bot/message/reply")
                    .header("Authorization", format!("Bearer {token}"))
                    .json(&reply)
                    .send().await;
            }
            _ => {}
        }
    }

    StatusCode::OK
}

fn line_welcome_flex() -> serde_json::Value {
    serde_json::json!({
        "type": "bubble",
        "hero": {
            "type": "image",
            "url": "https://makimaki.tokyo/sushi-original-2.jpg",
            "size": "full",
            "aspectRatio": "20:13",
            "aspectMode": "cover"
        },
        "body": {
            "type": "box",
            "layout": "vertical",
            "contents": [
                {"type":"text","text":"makimaki","weight":"bold","size":"xl","color":"#3A5A40"},
                {"type":"text","text":"麻布十番の細巻き専門テイクアウト店","size":"xs","color":"#999999","margin":"sm"},
                {"type":"separator","margin":"lg"},
                {"type":"text","text":"ようこそ！makimaki公式LINEへ。\nメニューの確認やオンライン注文ができます。","size":"sm","color":"#555555","margin":"lg","wrap":true}
            ]
        },
        "footer": {
            "type": "box",
            "layout": "vertical",
            "spacing": "sm",
            "contents": [
                {"type":"button","style":"primary","color":"#3A5A40","action":{"type":"message","label":"📋 メニューを見る","text":"メニュー"}},
                {"type":"button","style":"secondary","action":{"type":"message","label":"🛒 注文する","text":"注文"}},
                {"type":"button","style":"secondary","action":{"type":"message","label":"📍 アクセス","text":"アクセス"}}
            ]
        }
    })
}

fn line_menu_flex() -> serde_json::Value {
    serde_json::json!({
        "type": "bubble",
        "body": {
            "type": "box",
            "layout": "vertical",
            "contents": [
                {"type":"text","text":"makimaki メニュー","weight":"bold","size":"lg","color":"#3A5A40"},
                {"type":"separator","margin":"md"},
                {"type":"text","text":"🔴 マグロ","weight":"bold","size":"sm","margin":"lg"},
                {"type":"text","text":"赤身鉄火 ¥1,400 / 中とろ鉄火 ¥1,600\nネギトロ ¥1,400 / とろたく ¥1,400","size":"xs","color":"#666","wrap":true,"margin":"sm"},
                {"type":"text","text":"🐟 シーフード","weight":"bold","size":"sm","margin":"lg"},
                {"type":"text","text":"サーモン ¥900 / サーモンCC ¥900\nエビマヨパクチー ¥800 / いか大葉 ¥800\n生エビマヨ ¥900 / 小肌ガリ大葉 ¥900\nあなきゅう ¥900","size":"xs","color":"#666","wrap":true,"margin":"sm"},
                {"type":"text","text":"🥒 野菜・その他","weight":"bold","size":"sm","margin":"lg"},
                {"type":"text","text":"明太きゅうり ¥700 / ツナマヨ ¥700\n梅しそきゅう ¥650 / かっぱ ¥600\nわさびマヨきゅう ¥600 / 納豆白ねぎ ¥600\n梅かつお高菜 ¥600 / たまごマヨ ¥600\nたまごいなり ¥600 / かんぴょう ¥600\nおしんこ ¥500","size":"xs","color":"#666","wrap":true,"margin":"sm"},
                {"type":"separator","margin":"lg"},
                {"type":"text","text":"🎁 セットメニュー","weight":"bold","size":"sm","margin":"lg"},
                {"type":"text","text":"makimaki Box (4本) ¥3,500\nまぐろBox (4本) ¥5,000","size":"xs","color":"#666","wrap":true,"margin":"sm"},
                {"type":"text","text":"※価格は税別。箱代¥200","size":"xxs","color":"#999","margin":"md"}
            ]
        },
        "footer": {
            "type": "box",
            "layout": "vertical",
            "contents": [
                {"type":"button","style":"primary","color":"#3A5A40","action":{"type":"uri","label":"🛒 オンラインで注文する","uri":"https://makimaki.tokyo/#menu"}}
            ]
        }
    })
}

fn line_order_flex() -> serde_json::Value {
    serde_json::json!({
        "type": "bubble",
        "body": {
            "type": "box",
            "layout": "vertical",
            "contents": [
                {"type":"text","text":"オンライン注文","weight":"bold","size":"lg","color":"#3A5A40"},
                {"type":"text","text":"Webサイトからボックスを選んで\nお好みの巻きを詰められます。","size":"sm","color":"#666","wrap":true,"margin":"md"},
                {"type":"separator","margin":"lg"},
                {"type":"box","layout":"horizontal","margin":"lg","contents":[
                    {"type":"text","text":"受付時間","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"11:00 - 17:30","size":"xs","color":"#333","align":"end"}
                ]},
                {"type":"box","layout":"horizontal","margin":"sm","contents":[
                    {"type":"text","text":"受取方法","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"店頭受取（約15分）","size":"xs","color":"#333","align":"end"}
                ]},
                {"type":"box","layout":"horizontal","margin":"sm","contents":[
                    {"type":"text","text":"お支払い","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"カード決済 / 店頭払い","size":"xs","color":"#333","align":"end"}
                ]}
            ]
        },
        "footer": {
            "type": "box",
            "layout": "vertical",
            "spacing": "sm",
            "contents": [
                {"type":"button","style":"primary","color":"#3A5A40","action":{"type":"uri","label":"注文ページを開く","uri":"https://makimaki.tokyo/#menu"}},
                {"type":"button","style":"secondary","action":{"type":"uri","label":"電話で注文","uri":"tel:03-0000-0000"}}
            ]
        }
    })
}

fn line_access_flex() -> serde_json::Value {
    serde_json::json!({
        "type": "bubble",
        "body": {
            "type": "box",
            "layout": "vertical",
            "contents": [
                {"type":"text","text":"makimaki アクセス","weight":"bold","size":"lg","color":"#3A5A40"},
                {"type":"separator","margin":"md"},
                {"type":"box","layout":"horizontal","margin":"lg","contents":[
                    {"type":"text","text":"住所","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"東京都港区麻布十番2-19-1\nオフィスエイトビル 1F","size":"xs","color":"#333","align":"end","wrap":true}
                ]},
                {"type":"box","layout":"horizontal","margin":"md","contents":[
                    {"type":"text","text":"最寄駅","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"麻布十番駅 徒歩1分\n（南北線・大江戸線）","size":"xs","color":"#333","align":"end","wrap":true}
                ]},
                {"type":"box","layout":"horizontal","margin":"md","contents":[
                    {"type":"text","text":"営業時間","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"11:00 - 18:00","size":"xs","color":"#333","align":"end"}
                ]},
                {"type":"box","layout":"horizontal","margin":"md","contents":[
                    {"type":"text","text":"定休日","size":"xs","color":"#999","flex":0},
                    {"type":"text","text":"不定休","size":"xs","color":"#333","align":"end"}
                ]}
            ]
        },
        "footer": {
            "type": "box",
            "layout": "vertical",
            "spacing": "sm",
            "contents": [
                {"type":"button","style":"primary","color":"#3A5A40","action":{"type":"uri","label":"📍 Google Mapsで見る","uri":"https://maps.google.com/?q=東京都港区麻布十番2-19-1+オフィスエイトビル1F"}}
            ]
        }
    })
}
