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

    let stripe_key = std::env::var("STRIPE_SECRET_KEY").ok();
    let admin_key = std::env::var("ADMIN_KEY").expect("ADMIN_KEY environment variable must be set");

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
        .route("/api/requests", get(list_requests).post(create_request))
        .route("/api/requests/{id}/status", post(update_request_status))
        .route("/api/events", post(track_event).get(get_events))
        .route("/api/analytics", get(get_analytics))
        .route("/api/cloudprnt", post(cloudprnt_poll))
        .route("/api/cloudprnt/job", get(cloudprnt_job))
        .route("/api/orders/{id}/cancel", post(cancel_order))
        .route("/api/stripe-webhook", post(stripe_webhook))
        .route("/api/inventory", get(get_inventory))
        .route("/box", get(box_page))
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
                { eprintln!("Stripe error: {body}"); (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Payment processing failed"}))) }
            }
        }
        Err(e) => { eprintln!("Error: {e}"); (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Server error"}))) },
    }
}

async fn stripe_success(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SuccessQuery>,
) -> impl IntoResponse {
    if let Some(order_id) = &q.order_id {
        // Verify payment via Stripe API before marking as paid
        let verified = if let Some(stripe_key) = &state.stripe_key {
            let session_id: Option<String> = {
                let db = state.db.lock().unwrap();
                db.query_row(
                    "SELECT stripe_session FROM orders WHERE id=?1",
                    rusqlite::params![order_id], |r| r.get(0)
                ).ok()
            };
            if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
                let client = reqwest::Client::new();
                if let Ok(resp) = client
                    .get(&format!("https://api.stripe.com/v1/checkout/sessions/{sid}"))
                    .basic_auth(stripe_key, Option::<&str>::None)
                    .send().await
                {
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        body["payment_status"].as_str() == Some("paid")
                    } else { false }
                } else { false }
            } else { false }
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

async fn guide_page() -> Html<&'static str> {
    Html(include_str!("../static/guide.html"))
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
        "stripe_redirect", "order_nopay", "order_complete",
    ];
    if !ALLOWED.contains(&input.event.as_str()) {
        return StatusCode::BAD_REQUEST;
    }
    let data = input.data.unwrap_or_default();
    if data.len() > 100 { return StatusCode::BAD_REQUEST; }

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
                receipt.push_str(&format!("備考: {}\n\n", note));
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

    let (stripe_session, paid) = {
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

    // If paid via Stripe, attempt refund
    if paid != 0 && !stripe_session.is_empty() {
        if let Some(stripe_key) = &state.stripe_key {
            let client = reqwest::Client::new();
            // Get payment intent from session
            if let Ok(resp) = client
                .get(&format!("https://api.stripe.com/v1/checkout/sessions/{stripe_session}"))
                .basic_auth(stripe_key, Option::<&str>::None)
                .send().await
            {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(pi) = body["payment_intent"].as_str() {
                        let _ = client
                            .post("https://api.stripe.com/v1/refunds")
                            .basic_auth(stripe_key, Option::<&str>::None)
                            .form(&[("payment_intent", pi)])
                            .send().await;
                    }
                }
            }
        }
    }

    let db = state.db.lock().unwrap();
    let _ = db.execute("UPDATE orders SET status='cancelled', paid=0 WHERE id=?1", rusqlite::params![id]);
    (StatusCode::OK, Json(serde_json::json!({"ok": true, "refunded": paid != 0})))
}

// --- Stripe Webhook ---
async fn stripe_webhook(
    State(state): State<Arc<AppState>>,
    body: String,
) -> impl IntoResponse {
    // Parse the event (simplified - production should verify signature)
    let event: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    let event_type = event["type"].as_str().unwrap_or("");
    if event_type == "checkout.session.completed" {
        let session = &event["data"]["object"];
        if session["payment_status"].as_str() == Some("paid") {
            if let Some(order_id) = session["metadata"]["order_id"].as_str() {
                let db = state.db.lock().unwrap();
                let _ = db.execute("UPDATE orders SET paid=1 WHERE id=?1", rusqlite::params![order_id]);
            }
        }
    }
    StatusCode::OK
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

async fn box_page() -> Html<&'static str> {
    Html(include_str!("../static/box.html"))
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
