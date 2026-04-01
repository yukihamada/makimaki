use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tower_http::services::ServeDir;

struct AppState {
    db: Mutex<Connection>,
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
    created_at: String,
}

#[derive(Deserialize)]
struct UpdateStatus {
    status: String,
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
            created_at TEXT NOT NULL
        )",
    )
    .expect("Failed to create table");
}

#[tokio::main]
async fn main() {
    let db_path = std::env::var("DB_PATH").unwrap_or_else(|_| "makimaki.db".to_string());
    let conn = Connection::open(&db_path).expect("Failed to open DB");
    conn.execute_batch("PRAGMA journal_mode=WAL;").ok();
    init_db(&conn);

    let state = Arc::new(AppState {
        db: Mutex::new(conn),
    });

    let app = Router::new()
        .route("/api/orders", post(create_order).get(list_orders))
        .route("/api/orders/{id}/status", post(update_order_status))
        .route("/admin", get(admin_page))
        .fallback_service(ServeDir::new("static"))
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{port}");
    println!("makimaki listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn create_order(
    State(state): State<Arc<AppState>>,
    Json(input): Json<CreateOrder>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let total: u32 = input.items.iter().map(|i| i.price * i.qty).sum();
    let items_json = serde_json::to_string(&input.items).unwrap_or_default();
    let now = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .format("%Y-%m-%d %H:%M")
        .to_string();

    let db = state.db.lock().unwrap();
    match db.execute(
        "INSERT INTO orders (id,name,phone,pickup_time,items,note,total,status,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'new',?8)",
        rusqlite::params![id, input.name, input.phone, input.pickup_time, items_json, input.note, total, now],
    ) {
        Ok(_) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"id": id, "total": total})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

async fn list_orders(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare("SELECT id,name,phone,pickup_time,items,note,total,status,created_at FROM orders ORDER BY created_at DESC LIMIT 200")
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
                created_at: row.get(8)?,
            })
        })
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    Json(orders)
}

async fn update_order_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(input): Json<UpdateStatus>,
) -> impl IntoResponse {
    let db = state.db.lock().unwrap();
    match db.execute(
        "UPDATE orders SET status=?1 WHERE id=?2",
        rusqlite::params![input.status, id],
    ) {
        Ok(n) if n > 0 => StatusCode::OK,
        _ => StatusCode::NOT_FOUND,
    }
}

async fn admin_page() -> Html<&'static str> {
    Html(include_str!("../static/admin.html"))
}
