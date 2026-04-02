# makimaki

**麻布十番の細巻き専門テイクアウト店** のWebサイト + オンラインオーダー + 店舗POSシステム。

https://makimaki.tokyo

## Features

- **ボックスファースト注文** — 2本/4本/6本 Box を選び、中身の巻きを詰めるUI
- **オンライン決済** — Square Checkout API でカード決済
- **店舗POS画面** (`/pos`) — iPad最適化、現金/カード決済、Square Terminal連携
- **セルフオーダー** (`/order`) — 店頭iPad でお客様が自分で注文 → 番号札でレジ決済
- **POS自動通知** — セルフオーダーが入ると POS にチャイム音+トースト+自動切替
- **レシート印刷** — Star CloudPRNT でサーマルプリンター自動印刷
- **JA/EN切替** — 全テキスト日英対応
- **管理画面** (`/admin`) — 注文管理、在庫ON/OFF、営業切替、売上統計、アクセス解析
- **スタッフガイド** (`/guide`) — 運営マニュアル + 要望投稿

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust + axum 0.7 |
| Database | SQLite (rusqlite, WAL mode) |
| Payment | Square (Checkout API + Terminal API) |
| Hosting | Fly.io (nrt region) |
| Frontend | Vanilla HTML/CSS/JS (no framework) |
| Printer | Star mC-Print3 (CloudPRNT) |

## Architecture

```
                           ┌─────────────┐
  makimaki.tokyo           │  Fly.io     │
  ─────────────            │  (nrt)      │
                           │             │
  /          → お客様サイト   │  Rust axum  │──→ SQLite
  /order     → セルフオーダー │             │
  /pos?key=  → スタッフPOS   │             │──→ Square API
  /admin     → 管理画面      │             │
  /guide     → ガイド       │             │──→ Star CloudPRNT
                           └─────────────┘

  iPad #1 (/order) ──注文──→ DB ──5秒ポーリング──→ iPad #2 (/pos)
                                                  ↓ チャイム+トースト
                                                  ↓ スタッフが決済
                                                  ↓ レシート自動印刷
```

## Pages

| Path | Auth | Description |
|------|------|-------------|
| `/` | - | お客様向けサイト（ボックス注文 + Square決済） |
| `/order` | - | 店頭セルフオーダー（お客様用iPad） |
| `/pos?key=xxx` | Admin Key | 店舗POS（スタッフ用iPad） |
| `/admin` | Admin Key | 管理画面 |
| `/guide` | - | スタッフ運営ガイド |

## Local Development

```bash
# Prerequisites: Rust 1.75+
ADMIN_KEY=your-key cargo run

# With Square payments
SQUARE_ACCESS_TOKEN=xxx SQUARE_LOCATION_ID=xxx ADMIN_KEY=your-key cargo run
```

Open http://localhost:8080

## Deploy

```bash
fly deploy --remote-only -a makimaki

# Set secrets
fly secrets set SQUARE_ACCESS_TOKEN=xxx SQUARE_LOCATION_ID=xxx ADMIN_KEY=xxx -a makimaki
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `ADMIN_KEY` | Yes | Admin/POS authentication key |
| `SQUARE_ACCESS_TOKEN` | No | Square API access token |
| `SQUARE_LOCATION_ID` | No | Square location ID |
| `DB_PATH` | No | SQLite path (default: `makimaki.db`) |
| `PORT` | No | HTTP port (default: `8080`) |
| `BASE_URL` | No | Public URL (default: `https://makimaki.tokyo`) |

## API Endpoints

### Public
| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Main site |
| GET | `/order` | Self-order page |
| GET | `/guide` | Staff guide |
| GET | `/api/store-config` | Store open/close status |
| GET | `/api/menu-status` | Item availability |
| POST | `/api/orders` | Create order (self-order / pay at shop) |
| POST | `/api/checkout` | Create Square checkout session |
| POST | `/api/events` | Track analytics event |
| GET | `/api/square-callback` | Payment completion redirect |
| POST | `/api/square-webhook` | Square webhook receiver |

### Admin (requires `x-admin-key` header)
| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin` | Admin panel |
| GET | `/pos?key=xxx` | POS page |
| GET | `/api/orders` | List all orders |
| POST | `/api/orders/:id/status` | Update order status |
| POST | `/api/orders/:id/cancel` | Cancel & refund order |
| POST | `/api/pos/order` | Create POS order (cash/card) |
| POST | `/api/pos/terminal-checkout` | Send to Square Terminal |
| GET | `/api/pos/terminal-status/:id` | Poll terminal payment status |
| GET/POST | `/api/menu-status` | Get/set item availability |
| POST | `/api/store-config` | Update store settings |
| GET | `/api/stats` | Today's statistics |
| GET | `/api/analytics` | 30-day historical analytics |

## Security

- SQL injection: parameterized queries throughout
- XSS: all user input escaped via `textContent`
- Square verification: payment verified via Square API before marking paid
- Admin auth: secure key required for POS, admin, and all write APIs
- POS page: requires `?key=` parameter (not publicly accessible)
- Analytics: event whitelist, data length limit
- Error messages: internal details logged server-side only

## Links

- Site: https://makimaki.tokyo
- Instagram: [@maki_maki_tokyo](https://www.instagram.com/maki_maki_tokyo/)
- LINE: https://line.me/R/ti/p/@061xfqki
