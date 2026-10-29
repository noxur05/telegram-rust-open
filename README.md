# telegram-happ-rust

A Telegram bot for selling VPN subscriptions via cryptocurrency payments. Built with Rust, it integrates with **Marzban** (VPN panel) for subscription management and **NOWPayments** for crypto payment processing.

## Features

- **Crypto Payments** — Users top up their balance via NOWPayments (USDT, BTC, etc.)
- **VPN Subscription Purchase** — Atomic balance deduction with automatic Marzban account creation
- **Subscription Management** — Users can view their active subscriptions, traffic usage, and expiration
- **Payment Verification** — Manual and automatic payment status checking with stored pending payments
- **Admin Broadcast** — Admin can send messages to all users with batched delivery and progress reporting
- **Support System** — Users send support messages which are forwarded to a support group; admins reply directly
- **Rate Limiting** — Token bucket algorithm (15 requests/60s per user) to prevent abuse
- **Bot Menu Commands** — Persistent command menu (☰ button) with `/start`, `/help`, `/check`, `/support`

## Architecture

```
src/
├── main.rs              # Entry point, dispatcher, message/callback routing
├── config.rs            # Environment config with global cached singleton
├── db.rs                # All database operations (raw SQL via SeaORM)
├── commands.rs          # Command parser (/start, /help, /check, /support)
├── rate_limit.rs        # Token bucket rate limiter (in-memory)
├── utils.rs             # Shared utilities (escape_markdown_v2)
├── auth/
│   ├── mod.rs           # Global Marzban HTTP client singleton
│   ├── auth_client.rs   # JWT token management with auto-refresh
│   ├── http_client.rs   # Authenticated HTTP client with 401 retry
│   └── nowpayments.rs   # NOWPayments API client
└── handlers/
    ├── mod.rs           # Handler module declarations
    ├── buttons.rs       # All button labels and callback data constants
    ├── start.rs         # /start command, main menu keyboard
    ├── help.rs          # /help command with subscription info
    ├── balance.rs       # Balance top-up flow (create NOWPayments payment)
    ├── check.rs         # Payment verification (manual /check and button)
    ├── buy.rs           # Subscription purchase (deduct → Marzban → refund on failure)
    ├── subscriptions.rs # View user's Marzban subscriptions
    ├── broadcast.rs     # Admin broadcast with batching and retry
    └── support.rs       # User support message forwarding
```

## User Flow

```
1. /start → Main menu with keyboard buttons
2. "💳 Hasaby doldur" → Creates NOWPayments crypto payment → Shows address
3. User sends crypto to the address
4. "✅ Tölegi barla" → Checks payment status via NOWPayments API
5. If paid → Balance topped up atomically (with deduplication)
6. "🛒 Abunalyk satyn al" → Deducts balance → Creates Marzban VPN account
7. User receives subscription URL to add to their VPN app
```

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust (2021 edition) |
| Async Runtime | Tokio |
| Telegram Framework | teloxide 0.17 |
| Database | PostgreSQL via SeaORM (raw SQL) |
| HTTP Client | reqwest |
| VPN Panel | Marzban (REST API with JWT auth) |
| Payments | NOWPayments API |
| Serialization | serde_json |

## Database Schema

### Tables

**`users`** — Registered Telegram users
| Column | Type | Constraints |
|--------|------|------------|
| id | SERIAL | PRIMARY KEY |
| username | VARCHAR | NULLABLE |
| telegram_id | BIGINT | NOT NULL, UNIQUE |
| is_bot | BOOLEAN | NOT NULL, DEFAULT false |
| balance | NUMERIC | NOT NULL, DEFAULT 0 |
| created_at | TIMESTAMP | NOT NULL, DEFAULT timezone('Asia/Ashgabat', now()) |

**`transactions`** — Balance changes (top-ups, deductions, refunds)
| Column | Type | Constraints |
|--------|------|------------|
| id | SERIAL | PRIMARY KEY |
| user_id | INTEGER | NOT NULL, FK → users(id) |
| amount | NUMERIC | NOT NULL (positive = credit, negative = debit) |
| balance_after | NUMERIC | NOT NULL |
| description | VARCHAR | NULLABLE |
| payment_id | VARCHAR | NULLABLE (NULL for deductions/refunds) |
| created_at | TIMESTAMP | NOT NULL, DEFAULT timezone('Asia/Ashgabat', now()) |

**`pending_payments`** — Active payment sessions (auto-expires after 24h)
| Column | Type | Constraints |
|--------|------|------------|
| id | SERIAL | PRIMARY KEY |
| payment_id | VARCHAR | NOT NULL, UNIQUE |
| telegram_user_id | BIGINT | NOT NULL |
| created_at | TIMESTAMP | NOT NULL, DEFAULT timezone('Asia/Ashgabat', now()) |
| expires_at | TIMESTAMP | NOT NULL |

### Indexes

- `users_telegram_id_key` — UNIQUE on `users.telegram_id`
- `idx_pending_payments_telegram_user_id` — on `pending_payments.telegram_user_id`
- `idx_pending_payments_expires_at` — on `pending_payments.expires_at`
- `pending_payments_payment_id_key` — UNIQUE on `pending_payments.payment_id`

### Key DB Operations

- **`save_user_from_telegram`** — INSERT ON CONFLICT DO NOTHING (idempotent user registration)
- **`top_up_balance`** — Atomic CTE: checks for duplicate payment_id → inserts transaction → updates balance (all in one query)
- **`deduct_balance`** — Atomic UPDATE with `WHERE balance >= amount` (prevents overdraft race conditions)
- **`refund_balance`** — Atomic UPDATE + transaction record (used when Marzban creation fails after deduction)
- **`store_pending_payment`** — Replaces existing pending payment per user (DELETE + INSERT)

## Migrations

SQL migration files are in `migrations/`. Run them manually with `psql` when needed:

```bash
# Run a specific migration
psql -d telegram_bot -f migrations/001_create_users_table.sql

# Run all migrations in order
for f in migrations/*.sql; do
  echo "Running $f..."
  psql -d telegram_bot -f "$f"
done
```

### Migration Files

| File | Description |
|------|------------|
| `001_create_users_table.sql` | Creates `users` table with telegram_id unique constraint |
| `002_create_transactions_table.sql` | Creates `transactions` table with FK to users |
| `003_create_pending_payments_table.sql` | Creates `pending_payments` table with indexes |

## Environment Variables

Create a `.env` file (see `.env.example`):

```env
# Required
TELEGRAM_BOT_API=<bot_token_from_botfather>
DATABASE_URL=postgresql://user:password@localhost:5432/telegram_bot

# Marzban VPN Panel
MARZBAN_HOST=https://your-marzban-panel.com
MARZBAN_USERNAME=admin
MARZBAN_PASSWORD=admin_password
SUBSCRIPTION_URL=https://your-marzban-panel.com

# NOWPayments
NOWPAYMENTS_API_KEY=<your_api_key>

# Optional - Payment settings (defaults shown)
PRICE_AMOUNT=15.0              # Top-up amount in fiat
PRICE_CURRENCY=USD             # Fiat currency
PAY_CURRENCY=USDTBSC           # Crypto currency to accept
DESCRIPTION=VPN subscription top-up

# Optional - Subscription settings (defaults shown)
SUBSCRIPTION_PRICE=2.5         # Price per subscription in USD
SUBSCRIPTION_DAYS=31           # Subscription duration
SUBSCRIPTION_DATA_LIMIT_GB=100 # Data limit per subscription

# Optional - Marzban proxy settings (defaults shown)
MARZBAN_INBOUND_TAG=VLESS TCP REALITY
MARZBAN_PROXY_TYPE=vless

# Optional - Admin & Support
ADMIN_TELEGRAM_ID=<your_telegram_id>    # Enables broadcast feature
SUPPORT_CHAT_ID=<group_chat_id>         # Enables support forwarding
```

## Bot Commands

| Command | Description |
|---------|------------|
| `/start` | Start the bot, show main menu |
| `/help` | Show help with pricing and instructions |
| `/check <payment_id>` | Manually check a payment status |
| `/support` | Send a support message |

## Keyboard Buttons

| Button | Action |
|--------|--------|
| 💳 Hasaby doldur | Create a new crypto payment to top up balance |
| 🛒 Abunalyk satyn al | Purchase a VPN subscription (deducts from balance) |
| 📋 Meniň abunalyklarymy | View all your active/expired subscriptions |
| ❓ Kömek | Show help information |
| 📞 Goldaw | Send a support message |

## Build & Deploy

### Prerequisites

- Rust toolchain (for building only — not needed on server)
- PostgreSQL database
- Marzban VPN panel
- NOWPayments account

### Build

```bash
# Development
cargo build

# Production release
cargo build --release
# Binary: target/release/telegram-happ-rust
```

### Deploy to Server

```bash
# 1. Copy binary to server
scp target/release/telegram-happ-rust root@your-server:/root/rust/

# 2. Create .env file on server
# /root/rust/.env (with all required environment variables)

# 3. Set up database (first time only)
sudo -u postgres createdb telegram_bot
for f in migrations/*.sql; do
  sudo -u postgres psql -d telegram_bot -f "$f"
done

# 4. Create systemd service
cat > /etc/systemd/system/telegram-bot.service << EOF
[Unit]
Description=Telegram VPN Bot
After=network.target postgresql.service

[Service]
Type=simple
WorkingDirectory=/root/rust
ExecStart=/root/rust/telegram-happ-rust
Restart=always
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

# 5. Start service
systemctl daemon-reload
systemctl enable telegram-bot
systemctl start telegram-bot

# 6. Check logs
journalctl -u telegram-bot -f
```

### Update Deployment

```bash
# Build locally and upload
cargo build --release
scp target/release/telegram-happ-rust root@your-server:/root/rust/
ssh root@your-server "systemctl restart telegram-bot"
```

## Admin Features

### Broadcast

When `ADMIN_TELEGRAM_ID` is set, any plain text message from the admin (that isn't a command or button press) triggers the broadcast flow:

1. Bot shows confirmation with user count
2. Admin confirms or cancels
3. Messages are sent in batches of 25 with 1.1s pauses (stays under Telegram's 30 msg/sec limit)
4. Automatic retry on 429 (rate limit) errors (up to 3 retries)
5. Progress reported every 50 users
6. Final report shows sent/failed/total counts

### Support

When `SUPPORT_CHAT_ID` is set:

1. User taps "📞 Goldaw" → prompted to type their message
2. Message is forwarded to the support group with user info (name, username, ID)
3. Admin replies to the forwarded message in the group
4. Bot extracts the user ID and sends the reply back to the user

## Security

- **Atomic balance operations** — All balance changes use PostgreSQL atomic queries to prevent race conditions
- **Payment deduplication** — CTE-based check prevents double-processing of the same payment
- **Payment ownership validation** — Payments are validated against the user's telegram_id via order_id format (`tg_{telegram_id}_{timestamp}`)
- **Deduct-first pattern** — Balance is deducted before creating Marzban subscription; refunded on failure
- **JWT auto-refresh** — Marzban token is cached and auto-refreshed on expiry or 401
- **Rate limiting** — Token bucket per user prevents abuse
- **Bot filtering** — Messages and callbacks from bots are ignored
