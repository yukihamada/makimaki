# makimaki

**麻布十番の細巻き専門テイクアウト店** のWebサイト + オンラインオーダーシステム。

https://makimaki.fly.dev

## Features

- **細巻きメニュー** — セット2種 + おこのみ22品（実店舗メニュー準拠）
- **オンラインオーダー** — カート → 注文フォーム → 受取時間指定
- **Stripe決済** — カード決済 or 店頭払い選択
- **JA/EN切替** — 全テキスト日英対応
- **パララックス写真** — 9枚の店舗写真をスクロール演出
- **管理画面** (`/admin`) — 注文管理、在庫ON/OFF、営業切替、売上統計、アクセス解析、要望管理
- **スタッフガイド** (`/guide`) — 運営マニュアル + 要望投稿

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust + axum 0.7 |
| Database | SQLite (rusqlite, WAL mode) |
| Payment | Stripe Checkout |
| Hosting | Fly.io (nrt region) |
| Frontend | Vanilla HTML/CSS/JS (no framework) |

## Architecture

```
Browser ─── GET / ──→ Static HTML + JS
         ── POST /api/orders ──→ axum ──→ SQLite
         ── POST /api/checkout ──→ axum ──→ Stripe API ──→ redirect
         ── GET /admin ──→ Admin SPA (auth via x-admin-key header)
```

## Local Development

```bash
# Prerequisites: Rust 1.75+
cargo run

# With Stripe (optional)
STRIPE_SECRET_KEY=sk_test_xxx ADMIN_KEY=your-key cargo run
```

Open http://localhost:8080

## Deploy

```bash
# Fly.io (requires flyctl)
fly deploy --remote-only -a makimaki

# Set secrets
fly secrets set STRIPE_SECRET_KEY=sk_live_xxx ADMIN_KEY=xxx -a makimaki
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `ADMIN_KEY` | Yes | Admin panel authentication key |
| `STRIPE_SECRET_KEY` | No | Stripe secret key for card payments |
| `DB_PATH` | No | SQLite path (default: `makimaki.db`) |
| `PORT` | No | HTTP port (default: `8080`) |
| `BASE_URL` | No | Public URL (default: `https://makimaki.fly.dev`) |

## API Endpoints

### Public
| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Main site |
| GET | `/guide` | Staff guide |
| GET | `/api/store-config` | Store open/close status |
| GET | `/api/menu-status` | Item availability |
| POST | `/api/orders` | Create order (pay at shop) |
| POST | `/api/checkout` | Create Stripe checkout session |
| POST | `/api/events` | Track analytics event |

### Admin (requires `x-admin-key` header)
| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin` | Admin panel |
| GET | `/api/orders` | List all orders |
| POST | `/api/orders/:id/status` | Update order status |
| GET/POST | `/api/menu-status` | Get/set item availability |
| POST | `/api/store-config` | Update store settings |
| GET | `/api/stats` | Today's statistics |
| GET | `/api/events` | Analytics data |
| GET | `/api/analytics` | 30-day historical analytics |

## Security

- SQL injection: parameterized queries throughout
- XSS: all user input escaped via `textContent`
- Stripe verification: payment status verified via Stripe API before marking paid
- Admin auth: secure key required, no hardcoded defaults
- Analytics: event whitelist, data length limit
- Error messages: internal details logged server-side only

## Links

- Site: https://makimaki.fly.dev
- Instagram: [@maki_maki_tokyo](https://www.instagram.com/maki_maki_tokyo/)
- LINE: https://line.me/R/ti/p/@061xfqki
