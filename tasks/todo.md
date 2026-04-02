# makimaki 改修計画

## Phase 1: Stripe → Square 移行（バックエンド）
- [ ] 1-1. `Cargo.toml` — reqwest はそのまま使える（Square REST API）
- [ ] 1-2. `main.rs` — `/api/checkout` を Square Checkout API に差し替え
  - Square Orders API で注文作成 → Payment Link 生成 → リダイレクトURL返却
  - `POST https://connect.squareapis.com/v2/online-checkout/payment-links`
- [ ] 1-3. `/api/stripe-success` → `/api/square-callback` に変更（payment完了確認）
- [ ] 1-4. `/api/stripe-webhook` → Square webhook 対応
- [ ] 1-5. 注文キャンセル時の返金を Square Refunds API に差し替え
- [ ] 1-6. 環境変数: `STRIPE_SECRET_KEY` → `SQUARE_ACCESS_TOKEN`, `SQUARE_LOCATION_ID`
- [ ] 1-7. フロントエンド: checkout成功後のコールバックURL調整

## Phase 2: iPad店舗用POS画面（新規）
- [ ] 2-1. `static/pos.html` — iPad最適化のPOS画面を新規作成
  - ボックスファーストUI（2本/4本/6本）を流用
  - タッチ操作に最適化（大きいボタン、スワイプ不要）
  - 横向き(landscape)レイアウト: 左=メニュー選択、右=注文内容
- [ ] 2-2. 決済方法の選択
  - 「現金」「カード（Square Terminal連携）」「QR決済」ボタン
  - 現金の場合: お預かり金額入力 → おつり表示
  - カードの場合: Square Terminal API で端末に送信
- [ ] 2-3. レシート自動印刷
  - 注文確定時に `/api/cloudprnt` 経由で既存のStar CloudPRNTに印刷
  - 既存のレシートフォーマットをそのまま活用（printed フラグ管理済み）
- [ ] 2-4. 注文一覧ダッシュボード（POS画面内タブ）
  - 本日の注文一覧: new → preparing → ready のステータス管理
  - タップでステータス変更
- [ ] 2-5. `/pos` ルートを main.rs に追加

## Phase 3: 動作確認・デプロイ
- [ ] 3-1. ローカルでSquare Sandbox テスト
- [ ] 3-2. iPad実機でPOS画面確認
- [ ] 3-3. Fly.io secrets 更新 & デプロイ
